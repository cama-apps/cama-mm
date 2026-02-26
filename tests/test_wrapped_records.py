"""
Tests for personal records wrapped feature.

Tests service logic (records extraction, streaks, KDA, enrichment parsing, slide grouping)
and drawing smoke tests (basic render, worst records styling, N/A values).
"""

import io
import json
from datetime import datetime
from unittest.mock import MagicMock, patch

import pytest
from PIL import Image

from services.wrapped_service import (
    PersonalRecord,
    PersonalRecordsWrapped,
    WrappedService,
)
from utils.wrapped_drawing import (
    SLIDE_COLORS,
    WORST_LABEL_COLOR,
    draw_records_slide,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _make_match_row(
    match_id=1,
    hero_id=1,
    kills=10,
    deaths=3,
    assists=15,
    last_hits=200,
    denies=10,
    gpm=500,
    xpm=600,
    hero_damage=20000,
    tower_damage=3000,
    hero_healing=500,
    obs_placed=5,
    sen_placed=8,
    stuns=30.0,
    towers_killed=2,
    won=1,
    match_date="2026-01-15 20:00:00",
    duration_seconds=2400,
    valve_match_id=8000000001,
    enrichment_data=None,
    team_number=1,
    side="radiant",
    radiant_score=30,
    dire_score=20,
    discord_id=111,
):
    return {
        "match_id": match_id,
        "discord_id": discord_id,
        "hero_id": hero_id,
        "kills": kills,
        "deaths": deaths,
        "assists": assists,
        "last_hits": last_hits,
        "denies": denies,
        "gpm": gpm,
        "xpm": xpm,
        "hero_damage": hero_damage,
        "tower_damage": tower_damage,
        "hero_healing": hero_healing,
        "obs_placed": obs_placed,
        "sen_placed": sen_placed,
        "stuns": stuns,
        "towers_killed": towers_killed,
        "won": won,
        "match_date": match_date,
        "duration_seconds": duration_seconds,
        "valve_match_id": valve_match_id,
        "enrichment_data": enrichment_data,
        "team_number": team_number,
        "side": side,
        "radiant_score": radiant_score,
        "dire_score": dire_score,
    }


def _make_enrichment_json(
    account_id=76561198000000001,
    actions_per_min=150,
    courier_kills=2,
    pings=42,
    purchase_log=None,
    comeback=None,
    throw=None,
):
    """Build a minimal enrichment_data JSON string."""
    player = {
        "account_id": account_id,
        "actions_per_min": actions_per_min,
        "courier_kills": courier_kills,
        "pings": pings,
    }
    if purchase_log is not None:
        player["purchase_log"] = purchase_log
    data = {"players": [player]}
    if comeback is not None:
        data["comeback"] = comeback
    if throw is not None:
        data["throw"] = throw
    return json.dumps(data)


def _build_service():
    """Build a WrappedService with mocked repositories."""
    wrapped_repo = MagicMock()
    player_repo = MagicMock()
    match_repo = MagicMock()
    bet_repo = MagicMock()
    svc = WrappedService(wrapped_repo, player_repo, match_repo, bet_repo)
    return svc, wrapped_repo, player_repo


def _sample_rows(count=6):
    """Generate `count` match rows with varying stats."""
    rows = []
    for i in range(count):
        rows.append(_make_match_row(
            match_id=i + 1,
            hero_id=i + 1,
            kills=5 + i * 3,
            deaths=2 + i,
            assists=10 + i * 2,
            last_hits=100 + i * 30,
            gpm=400 + i * 50,
            xpm=500 + i * 40,
            hero_damage=15000 + i * 3000,
            tower_damage=1000 + i * 500,
            towers_killed=i,
            won=1 if i % 2 == 0 else 0,
            match_date=f"2026-01-{10 + i:02d} 20:00:00",
            valve_match_id=8000000000 + i,
            duration_seconds=1800 + i * 300,
            radiant_score=25 + i,
            dire_score=20 + i,
        ))
    return rows


# ---------------------------------------------------------------------------
# Service Tests
# ---------------------------------------------------------------------------

class TestGetPlayerRecordsBasic:
    def test_basic_records_populated(self):
        svc, wrapped_repo, player_repo = _build_service()
        rows = _sample_rows(6)
        wrapped_repo.get_player_year_matches.return_value = rows

        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        player_repo.get_steam_ids.return_value = []

        result = svc.get_player_records_wrapped(111, "2026-01", guild_id=0)

        assert result is not None
        assert result.discord_username == "TestPlayer"
        assert result.games_played == 6
        assert len(result.records) > 0

        # Should have best kills record
        kills_best = [r for r in result.records if r.stat_key == "kills_best"]
        assert len(kills_best) == 1
        # Max kills is 5 + 5*3 = 20
        assert kills_best[0].value == 20

    def test_below_min_games_returns_none(self):
        svc, wrapped_repo, player_repo = _build_service()
        # Only 2 rows, below WRAPPED_MIN_GAMES (3)
        wrapped_repo.get_player_year_matches.return_value = _sample_rows(2)

        result = svc.get_player_records_wrapped(111, "2026-01", guild_id=0)
        assert result is None

    def test_worst_records_included(self):
        svc, wrapped_repo, player_repo = _build_service()
        rows = _sample_rows(6)
        wrapped_repo.get_player_year_matches.return_value = rows

        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        player_repo.get_steam_ids.return_value = []

        result = svc.get_player_records_wrapped(111, "2026-01", guild_id=0)
        worst_records = [r for r in result.records if r.is_worst]
        assert len(worst_records) > 0

        # Check specific worst records
        worst_keys = {r.stat_key for r in worst_records}
        assert "kda_worst" in worst_keys
        assert "gpm_worst" in worst_keys
        assert "deaths_worst" in worst_keys


class TestStreakComputation:
    def test_wwlwwwlll(self):
        """WWLWWWLLL -> (3, 3)"""
        matches = [
            {"won": True}, {"won": True}, {"won": False},
            {"won": True}, {"won": True}, {"won": True},
            {"won": False}, {"won": False}, {"won": False},
        ]
        win, lose = WrappedService._compute_streaks(matches)
        assert win == 3
        assert lose == 3

    def test_all_wins(self):
        matches = [{"won": True}] * 5
        win, lose = WrappedService._compute_streaks(matches)
        assert win == 5
        assert lose == 0

    def test_all_losses(self):
        matches = [{"won": False}] * 4
        win, lose = WrappedService._compute_streaks(matches)
        assert win == 0
        assert lose == 4

    def test_alternating(self):
        matches = [{"won": True}, {"won": False}, {"won": True}, {"won": False}]
        win, lose = WrappedService._compute_streaks(matches)
        assert win == 1
        assert lose == 1

    def test_empty(self):
        win, lose = WrappedService._compute_streaks([])
        assert win == 0
        assert lose == 0

    def test_none_won_resets_streaks(self):
        matches = [{"won": True}, {"won": None}, {"won": True}]
        win, lose = WrappedService._compute_streaks(matches)
        # None resets both counters — unknown result breaks streaks
        assert win == 1
        assert lose == 0


class TestKDARatioNoDeaths:
    def test_deaths_zero_uses_max_1(self):
        svc, wrapped_repo, player_repo = _build_service()
        rows = [
            _make_match_row(match_id=i + 1, kills=10, deaths=0, assists=5)
            for i in range(5)
        ]
        wrapped_repo.get_player_year_matches.return_value = rows

        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        player_repo.get_steam_ids.return_value = []

        result = svc.get_player_records_wrapped(111, "2026-01", guild_id=0)
        kda_best = [r for r in result.records if r.stat_key == "kda_best"]
        assert len(kda_best) == 1
        # (10 + 5) / max(0, 1) = 15.0
        assert kda_best[0].value == 15.0


class TestEnrichmentDataParsing:
    def test_enrichment_stats_extracted(self):
        svc, wrapped_repo, player_repo = _build_service()
        steam_id = 76561198000000001
        enrichment = _make_enrichment_json(
            account_id=steam_id,
            actions_per_min=200,
            courier_kills=3,
            pings=100,
            purchase_log=[{"key": "rapier", "time": 1800}, {"key": "rapier", "time": 2100}],
            comeback=15000,
            throw=8000,
        )
        rows = [
            _make_match_row(match_id=i + 1, enrichment_data=enrichment)
            for i in range(5)
        ]
        wrapped_repo.get_player_year_matches.return_value = rows

        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        player_repo.get_steam_ids.return_value = [steam_id]

        result = svc.get_player_records_wrapped(111, "2026-01", guild_id=0)
        keys = {r.stat_key for r in result.records}

        assert "apm_best" in keys
        assert "courier_kills_best" in keys
        assert "pings_worst" in keys
        assert "rapiers_best" in keys
        assert "comeback_best" in keys
        assert "throw_worst" in keys

        # Verify values
        apm = [r for r in result.records if r.stat_key == "apm_best"][0]
        assert apm.value == 200

        rapiers = [r for r in result.records if r.stat_key == "rapiers_best"][0]
        assert rapiers.value == 2

    def test_enrichment_data_missing_graceful(self):
        svc, wrapped_repo, player_repo = _build_service()
        rows = [
            _make_match_row(match_id=i + 1, enrichment_data=None)
            for i in range(5)
        ]
        wrapped_repo.get_player_year_matches.return_value = rows

        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        player_repo.get_steam_ids.return_value = [123]

        result = svc.get_player_records_wrapped(111, "2026-01", guild_id=0)
        # Should still produce records, enrichment stats should be N/A placeholders
        assert result is not None
        enrichment_keys = {"apm_best", "courier_kills_best", "pings_worst", "rapiers_best", "comeback_best", "throw_worst"}
        found_keys = {r.stat_key for r in result.records}
        # All enrichment keys present as N/A placeholders
        assert enrichment_keys.issubset(found_keys)
        for r in result.records:
            if r.stat_key in enrichment_keys:
                assert r.value is None
                assert r.display_value == "N/A"


class TestSlideGrouping:
    def test_get_slides_returns_5_slides(self):
        svc, wrapped_repo, player_repo = _build_service()
        # Build rows with enrichment data too
        steam_id = 76561198000000001
        enrichment = _make_enrichment_json(
            account_id=steam_id,
            actions_per_min=200,
            courier_kills=3,
            pings=100,
            purchase_log=[{"key": "rapier", "time": 1800}],
            comeback=15000,
            throw=8000,
        )
        rows = [
            _make_match_row(
                match_id=i + 1,
                hero_id=i + 1,
                kills=5 + i * 3,
                deaths=2 + i,
                assists=10 + i * 2,
                last_hits=100 + i * 30,
                gpm=400 + i * 50,
                xpm=500 + i * 40,
                hero_damage=15000 + i * 3000,
                tower_damage=1000 + i * 500,
                hero_healing=500 + i * 200,
                obs_placed=3 + i,
                sen_placed=5 + i,
                stuns=20.0 + i * 5,
                towers_killed=i,
                won=1 if i % 2 == 0 else 0,
                match_date=f"2026-01-{10 + i:02d} 20:00:00",
                valve_match_id=8000000000 + i,
                duration_seconds=1800 + i * 300,
                radiant_score=25 + i,
                dire_score=20 + i,
                enrichment_data=enrichment,
            )
            for i in range(6)
        ]
        wrapped_repo.get_player_year_matches.return_value = rows

        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        player_repo.get_steam_ids.return_value = [steam_id]

        result = svc.get_player_records_wrapped(111, "2026-01", guild_id=0)
        slides = result.get_slides()

        assert len(slides) == 5
        titles = [s[0] for s in slides]
        assert "Combat" in titles
        assert "Farming" in titles
        assert "Impact" in titles
        assert "Vision & Utility" in titles
        assert "Endurance & Streaks" in titles


# ---------------------------------------------------------------------------
# Repository Integration Test
# ---------------------------------------------------------------------------

TEST_GUILD_ID = 12345


class TestGetPlayerYearMatchesRepo:
    """Integration test exercising the actual SQL query."""

    def test_returns_matches_in_date_range(self, repo_db_path):
        from repositories.match_repository import MatchRepository
        from repositories.wrapped_repository import WrappedRepository

        match_repo = MatchRepository(repo_db_path)
        wrapped_repo = WrappedRepository(repo_db_path)

        # Record 3 matches with enrichment stats
        for i in range(3):
            match_id = match_repo.record_match(
                team1_ids=[100],
                team2_ids=[200],
                winning_team=1,
                guild_id=TEST_GUILD_ID,
            )
            match_repo.update_participant_stats(
                match_id=match_id,
                discord_id=100,
                hero_id=i + 1,
                kills=10 + i,
                deaths=3,
                assists=5,
                gpm=500,
                xpm=600,
                hero_damage=20000,
                tower_damage=3000,
                last_hits=200,
                denies=10,
                net_worth=15000,
            )

        from datetime import datetime, timezone
        # Year start to end of Dec 2026 (matches have match_date=now)
        end_ts = int(datetime(2026, 12, 31, 23, 59, 59, tzinfo=timezone.utc).timestamp()) + 1

        rows = wrapped_repo.get_player_year_matches(100, TEST_GUILD_ID, 2026, end_ts)
        assert len(rows) == 3
        assert all(r["discord_id"] == 100 for r in rows)
        kills_set = {r["kills"] for r in rows}
        assert kills_set == {10, 11, 12}
        # Guild isolation: different guild should return nothing
        rows_other = wrapped_repo.get_player_year_matches(100, 99999, 2026, end_ts)
        assert len(rows_other) == 0


# ---------------------------------------------------------------------------
# Drawing Smoke Tests
# ---------------------------------------------------------------------------

def _make_sample_records():
    """Build a list of PersonalRecords for drawing tests."""
    return [
        PersonalRecord("kills_best", "Most Kills", 25, "25 kills", 1, 100, 8000000001, "2026-01-15"),
        PersonalRecord("assists_best", "Most Assists", 30, "30 assists", 2, 101, 8000000002, "2026-01-16"),
        PersonalRecord("kda_best", "Best KDA", 12.5, "12.50 KDA", 3, 102, 8000000003, "2026-01-17"),
        PersonalRecord("deaths_worst", "Feeding Frenzy", 18, "18 deaths", 4, 103, 8000000004, "2026-01-18", is_worst=True),
        PersonalRecord("kda_worst", "Clown Fiesta", 0.5, "0.50 KDA", 5, 104, 8000000005, "2026-01-19", is_worst=True),
    ]


class TestDrawRecordsSlideBasic:
    @patch("utils.drawing._fetch_hero_image", return_value=None)
    def test_basic_render_dimensions(self, _mock_fetch):
        records = _make_sample_records()
        buf = draw_records_slide(
            slide_title="Combat",
            accent_color=SLIDE_COLORS["combat"],
            records=records,
            username="TestPlayer",
            month_name="January 2026",
            slide_number=1,
            total_slides=5,
            hero_names={1: "Anti-Mage", 2: "Axe", 3: "Bane", 4: "Bloodseeker", 5: "Crystal Maiden"},
        )
        img = Image.open(buf)
        assert img.size == (800, 600)
        assert img.mode == "RGBA"

    @patch("utils.drawing._fetch_hero_image", return_value=None)
    def test_worst_records_render(self, _mock_fetch):
        """Worst records should render without error."""
        records = [
            PersonalRecord("deaths_worst", "Feeding Frenzy", 18, "18 deaths", 4, 103, 8000000004, "2026-01-18", is_worst=True),
            PersonalRecord("kda_worst", "Clown Fiesta", 0.5, "0.50 KDA", 5, 104, 8000000005, "2026-01-19", is_worst=True),
        ]
        buf = draw_records_slide(
            slide_title="Combat",
            accent_color=SLIDE_COLORS["combat"],
            records=records,
            username="TestPlayer",
            month_name="January 2026",
            slide_number=1,
            total_slides=5,
            hero_names={4: "Bloodseeker", 5: "Crystal Maiden"},
        )
        img = Image.open(buf)
        assert img.size == (800, 600)

    @patch("utils.drawing._fetch_hero_image", return_value=None)
    def test_na_values_render(self, _mock_fetch):
        """N/A records should render dimmed."""
        records = [
            PersonalRecord("apm_best", "Highest APM", None, "N/A", None, None, None, None),
            PersonalRecord("courier_kills_best", "Most Courier Kills", None, "N/A", None, None, None, None),
        ]
        buf = draw_records_slide(
            slide_title="Vision & Utility",
            accent_color=SLIDE_COLORS["vision"],
            records=records,
            username="TestPlayer",
            month_name="January 2026",
            slide_number=4,
            total_slides=5,
            hero_names={},
        )
        img = Image.open(buf)
        assert img.size == (800, 600)
