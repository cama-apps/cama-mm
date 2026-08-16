"""Tests for the informational dota play-time aggregation (utils/playtime.py)."""

from datetime import date
from types import SimpleNamespace

import pytest

from utils.playtime import (
    aggregate_popular_hours,
    effective_timezone,
    format_hour_ranges,
    parse_hour_set,
)


def _player(hours, timezone=None):
    return SimpleNamespace(dota_play_hours=hours, timezone=timezone)


class TestParseHourSet:
    def test_single_hours(self):
        assert parse_hour_set("14,20,21") == {14, 20, 21}

    def test_simple_range(self):
        assert parse_hour_set("18-21") == {18, 19, 20, 21}

    def test_wrapping_range(self):
        assert parse_hour_set("22-2") == {22, 23, 0, 1, 2}

    def test_mixed_ranges_and_singles(self):
        assert parse_hour_set("14, 18-20, 23") == {14, 18, 19, 20, 23}

    def test_single_hour_range_collapses(self):
        assert parse_hour_set("9-9") == {9}

    @pytest.mark.parametrize("text", ["", "  ", "24", "18-24", "abc", "18--20", "-1"])
    def test_invalid_input_raises(self, text):
        with pytest.raises(ValueError):
            parse_hour_set(text)


class TestFormatHourRanges:
    def test_empty_set(self):
        assert format_hour_ranges(set()) == "none"

    def test_single_hour(self):
        assert format_hour_ranges({14}) == "14:00"

    def test_contiguous_range(self):
        assert format_hour_ranges({18, 19, 20, 21}) == "18:00-21:00"

    def test_multiple_disjoint_ranges(self):
        assert format_hour_ranges({9, 14, 15, 16, 20}) == "09:00, 14:00-16:00, 20:00"


class TestEffectiveTimezone:
    def test_uses_player_timezone_when_set(self):
        assert effective_timezone(_player([18], timezone="Asia/Tokyo")) == "Asia/Tokyo"

    def test_falls_back_to_default_when_unset(self):
        from utils.timezone import DEFAULT_TIMEZONE

        assert effective_timezone(_player([18], timezone=None)) == DEFAULT_TIMEZONE


class TestAggregatePopularHours:
    def test_counts_hours_for_players_in_reporting_timezone(self):
        players = [
            _player([20, 21], timezone="America/New_York"),
            _player([20], timezone="America/New_York"),
        ]

        counts = aggregate_popular_hours(
            players, reporting_timezone="America/New_York", today=date(2026, 1, 1)
        )

        assert counts[20] == 2
        assert counts[21] == 1
        assert sum(counts) == 3

    def test_converts_across_timezones(self):
        """8pm JST lands in a different reporting hour once converted to EST."""
        players = [_player([20], timezone="Asia/Tokyo")]  # 8pm JST (UTC+9)

        counts = aggregate_popular_hours(
            players, reporting_timezone="America/New_York", today=date(2026, 1, 1)
        )

        # 8pm JST on 2026-01-01 is 6am EST the same calendar day (winter, UTC-5).
        assert counts[6] == 1
        assert counts[20] == 0

    def test_ignores_players_without_hours_set(self):
        players = [_player(None), _player([]), _player([9])]

        counts = aggregate_popular_hours(players, reporting_timezone="America/New_York")

        assert sum(counts) == 1
        assert counts[9] == 1

    def test_unknown_reporting_timezone_falls_back_to_default(self):
        players = [_player([12], timezone="America/New_York")]

        counts = aggregate_popular_hours(
            players, reporting_timezone="Not/AZone", today=date(2026, 1, 1)
        )

        assert counts[12] == 1
