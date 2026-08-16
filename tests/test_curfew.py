"""Tests for named curfew windows (utils/curfew.py)."""

from datetime import datetime
from types import SimpleNamespace
from zoneinfo import ZoneInfo

import pytest

from utils.curfew import (
    effective_timezone,
    find_active_window,
    format_window,
    is_within_window,
    parse_clock,
)
from utils.timezone import DEFAULT_TIMEZONE


def _window(name="w", start_hour=22, start_minute=0, end_hour=6, end_minute=0, timezone=None):
    """A minimal stand-in for a CurfewWindow (only these attrs are read)."""
    return SimpleNamespace(
        name=name,
        start_hour=start_hour,
        start_minute=start_minute,
        end_hour=end_hour,
        end_minute=end_minute,
        timezone=timezone,
    )


class TestIsWithinWindow:
    def test_overnight_window_blocks_after_start(self):
        """22:00-06:00 window: 11pm is inside it."""
        window = _window(start_hour=22, end_hour=6)
        now = datetime(2026, 1, 1, 23, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_window(window, now=now) is True

    def test_overnight_window_blocks_before_end(self):
        """22:00-06:00 window: 3am (past midnight) is still inside it."""
        window = _window(start_hour=22, end_hour=6)
        now = datetime(2026, 1, 2, 3, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_window(window, now=now) is True

    def test_overnight_window_allows_after_end(self):
        window = _window(start_hour=22, end_hour=6)
        now = datetime(2026, 1, 2, 7, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_window(window, now=now) is False

    def test_overnight_window_allows_before_start(self):
        window = _window(start_hour=22, end_hour=6)
        now = datetime(2026, 1, 1, 21, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_window(window, now=now) is False

    def test_same_day_window(self):
        """09:00-17:00 window (start < end): only blocks inside that same-day span."""
        window = _window(start_hour=9, end_hour=17)
        inside = datetime(2026, 1, 1, 12, 0, tzinfo=ZoneInfo("America/New_York"))
        outside = datetime(2026, 1, 1, 20, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_window(window, now=inside) is True
        assert is_within_window(window, now=outside) is False

    def test_equal_start_and_end_never_blocks(self):
        window = _window(start_hour=22, start_minute=0, end_hour=22, end_minute=0)
        now = datetime(2026, 1, 1, 22, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_window(window, now=now) is False

    def test_window_timezone_overrides_general_timezone(self):
        window = _window(start_hour=22, end_hour=6, timezone="America/New_York")
        utc_now = datetime(2026, 1, 2, 3, 30, tzinfo=ZoneInfo("UTC"))  # 10:30pm EST
        assert is_within_window(window, general_timezone="Asia/Tokyo", now=utc_now) is True

    def test_falls_back_to_general_timezone_when_window_has_none(self):
        window = _window(start_hour=22, end_hour=6, timezone=None)
        utc_now = datetime(2026, 1, 1, 13, 30, tzinfo=ZoneInfo("UTC"))  # 10:30pm JST
        assert is_within_window(window, general_timezone="Asia/Tokyo", now=utc_now) is True

    def test_falls_back_to_default_when_both_unset(self):
        window = _window(start_hour=22, end_hour=6, timezone=None)
        moment_default_tz = datetime(2026, 1, 1, 22, 30, tzinfo=ZoneInfo(DEFAULT_TIMEZONE))
        assert is_within_window(window, now=moment_default_tz.astimezone(ZoneInfo("UTC"))) is True

    def test_unknown_timezone_falls_back_to_default(self):
        window = _window(start_hour=22, end_hour=6, timezone="Not/AZone")
        moment_default_tz = datetime(2026, 1, 1, 22, 30, tzinfo=ZoneInfo(DEFAULT_TIMEZONE))
        assert is_within_window(window, now=moment_default_tz.astimezone(ZoneInfo("UTC"))) is True


class TestFindActiveWindow:
    def test_returns_none_when_no_windows_active(self):
        windows = [_window(name="work", start_hour=9, end_hour=17)]
        now = datetime(2026, 1, 1, 20, 0, tzinfo=ZoneInfo("America/New_York"))
        assert find_active_window(windows, now=now) is None

    def test_returns_matching_window(self):
        windows = [
            _window(name="work", start_hour=9, end_hour=17),
            _window(name="sleep", start_hour=22, end_hour=6),
        ]
        now = datetime(2026, 1, 1, 23, 0, tzinfo=ZoneInfo("America/New_York"))
        match = find_active_window(windows, now=now)
        assert match.name == "sleep"

    def test_returns_alphabetically_first_when_multiple_active(self):
        windows = [
            _window(name="zzz", start_hour=0, start_minute=0, end_hour=23, end_minute=59),
            _window(name="aaa", start_hour=0, start_minute=0, end_hour=23, end_minute=59),
        ]
        now = datetime(2026, 1, 1, 12, 0, tzinfo=ZoneInfo("America/New_York"))
        match = find_active_window(windows, now=now)
        assert match.name == "aaa"


class TestParseClock:
    @pytest.mark.parametrize(
        "text,expected",
        [
            ("22:00", (22, 0)),
            ("6:00", (6, 0)),
            ("06:00", (6, 0)),
            ("0:00", (0, 0)),
            ("23:59", (23, 59)),
        ],
    )
    def test_valid_formats(self, text, expected):
        assert parse_clock(text) == expected

    @pytest.mark.parametrize("text", ["24:00", "10pm", "22:60", "-1:00", "", "10:0"])
    def test_invalid_formats_raise(self, text):
        with pytest.raises(ValueError):
            parse_clock(text)


class TestFormatWindow:
    def test_formats_name_am_pm_and_timezone(self):
        window = _window(
            name="work", start_hour=9, start_minute=0, end_hour=17, end_minute=30,
            timezone="America/New_York",
        )
        assert format_window(window) == '"work": 9:00 AM - 5:30 PM America/New_York'

    def test_falls_back_to_general_timezone(self):
        window = _window(name="sleep", start_hour=22, end_hour=6, timezone=None)
        assert (
            format_window(window, general_timezone="Asia/Tokyo")
            == '"sleep": 10:00 PM - 6:00 AM Asia/Tokyo'
        )

    def test_falls_back_to_default_timezone(self):
        window = _window(name="sleep", start_hour=22, end_hour=6, timezone=None)
        assert format_window(window) == f'"sleep": 10:00 PM - 6:00 AM {DEFAULT_TIMEZONE}'


class TestEffectiveTimezone:
    def test_window_timezone_wins(self):
        window = _window(timezone="America/New_York")
        assert effective_timezone(window, "Asia/Tokyo") == "America/New_York"

    def test_falls_back_to_general(self):
        window = _window(timezone=None)
        assert effective_timezone(window, "Asia/Tokyo") == "Asia/Tokyo"

    def test_falls_back_to_default(self):
        window = _window(timezone=None)
        assert effective_timezone(window, None) == DEFAULT_TIMEZONE
