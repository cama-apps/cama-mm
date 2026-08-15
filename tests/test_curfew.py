"""Tests for the personal lobby-queue curfew window (utils/curfew.py)."""

from datetime import datetime
from types import SimpleNamespace
from zoneinfo import ZoneInfo

import pytest

from utils.curfew import (
    DEFAULT_CURFEW_TIMEZONE,
    format_curfew_window,
    is_valid_timezone,
    is_within_curfew,
    parse_clock,
)


def _player(
    enabled=True,
    curfew_hour=22,
    curfew_minute=0,
    wake_hour=6,
    wake_minute=0,
    curfew_timezone=None,
    timezone=None,
):
    """A minimal stand-in for a Player (is_within_curfew only reads curfew_*/timezone attrs)."""
    return SimpleNamespace(
        curfew_enabled=enabled,
        curfew_hour=curfew_hour,
        curfew_minute=curfew_minute,
        curfew_wake_hour=wake_hour,
        curfew_wake_minute=wake_minute,
        curfew_timezone=curfew_timezone,
        timezone=timezone,
    )


class TestIsWithinCurfew:
    def test_disabled_curfew_never_blocks(self):
        player = _player(enabled=False)
        now = datetime(2026, 1, 1, 3, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=now) is False

    def test_unset_hours_never_blocks(self):
        player = _player(curfew_hour=None, wake_hour=None)
        now = datetime(2026, 1, 1, 23, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=now) is False

    def test_overnight_window_blocks_after_bedtime(self):
        """10pm-6am window: 11pm is inside it."""
        player = _player(curfew_hour=22, wake_hour=6)
        now = datetime(2026, 1, 1, 23, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=now) is True

    def test_overnight_window_blocks_before_wake(self):
        """10pm-6am window: 3am (past midnight) is still inside it."""
        player = _player(curfew_hour=22, wake_hour=6)
        now = datetime(2026, 1, 2, 3, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=now) is True

    def test_overnight_window_allows_after_wake(self):
        """10pm-6am window: 7am is outside it."""
        player = _player(curfew_hour=22, wake_hour=6)
        now = datetime(2026, 1, 2, 7, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=now) is False

    def test_overnight_window_allows_before_bedtime(self):
        """10pm-6am window: 9pm is outside it."""
        player = _player(curfew_hour=22, wake_hour=6)
        now = datetime(2026, 1, 1, 21, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=now) is False

    def test_same_day_window(self):
        """1am-5am window (bedtime < wake): only blocks inside that same-day span."""
        player = _player(curfew_hour=1, wake_hour=5)
        inside = datetime(2026, 1, 1, 3, 0, tzinfo=ZoneInfo("America/New_York"))
        outside = datetime(2026, 1, 1, 12, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=inside) is True
        assert is_within_curfew(player, now=outside) is False

    def test_equal_bedtime_and_wake_never_blocks(self):
        player = _player(curfew_hour=22, curfew_minute=0, wake_hour=22, wake_minute=0)
        now = datetime(2026, 1, 1, 22, 0, tzinfo=ZoneInfo("America/New_York"))
        assert is_within_curfew(player, now=now) is False

    def test_converts_from_utc_using_players_timezone(self):
        """10pm EST is 3am UTC (winter, no DST) — the check must convert, not compare raw UTC."""
        player = _player(curfew_hour=22, wake_hour=6, curfew_timezone="America/New_York")
        utc_now = datetime(2026, 1, 2, 3, 30, tzinfo=ZoneInfo("UTC"))  # 10:30pm EST
        assert is_within_curfew(player, now=utc_now) is True

    def test_unknown_timezone_falls_back_to_default(self):
        player = _player(curfew_hour=22, wake_hour=6, curfew_timezone="Not/AZone")
        # 10:30pm in the default timezone, expressed as its UTC equivalent.
        moment_default_tz = datetime(2026, 1, 1, 22, 30, tzinfo=ZoneInfo(DEFAULT_CURFEW_TIMEZONE))
        assert is_within_curfew(player, now=moment_default_tz.astimezone(ZoneInfo("UTC"))) is True


class TestTimezoneFallbackChain:
    """curfew_timezone (override) > player.timezone (general) > DEFAULT_CURFEW_TIMEZONE."""

    def test_falls_back_to_general_timezone_when_curfew_timezone_unset(self):
        player = _player(curfew_hour=22, wake_hour=6, curfew_timezone=None, timezone="Asia/Tokyo")
        # 10:30pm JST — should register as inside curfew only under the Tokyo interpretation.
        utc_now = datetime(2026, 1, 1, 13, 30, tzinfo=ZoneInfo("UTC"))  # 10:30pm JST (UTC+9)
        assert is_within_curfew(player, now=utc_now) is True

    def test_curfew_timezone_override_wins_over_general_timezone(self):
        player = _player(
            curfew_hour=22, wake_hour=6, curfew_timezone="America/New_York", timezone="Asia/Tokyo"
        )
        # 13:30 UTC is 8:30am EST (winter) — outside the 10pm-6am EST window,
        # proving the explicit curfew_timezone override, not the general one, wins.
        utc_now = datetime(2026, 1, 1, 13, 30, tzinfo=ZoneInfo("UTC"))
        assert is_within_curfew(player, now=utc_now) is False

    def test_falls_back_to_default_when_both_unset(self):
        player = _player(curfew_hour=22, wake_hour=6, curfew_timezone=None, timezone=None)
        moment_default_tz = datetime(2026, 1, 1, 22, 30, tzinfo=ZoneInfo(DEFAULT_CURFEW_TIMEZONE))
        assert is_within_curfew(player, now=moment_default_tz.astimezone(ZoneInfo("UTC"))) is True

    def test_format_falls_back_to_general_timezone(self):
        player = _player(
            curfew_hour=22, curfew_minute=0, wake_hour=6, wake_minute=0,
            curfew_timezone=None, timezone="Asia/Tokyo",
        )
        assert format_curfew_window(player) == "10:00 PM - 6:00 AM Asia/Tokyo"


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


class TestFormatCurfewWindow:
    def test_formats_am_pm_and_timezone(self):
        player = _player(
            curfew_hour=22, curfew_minute=0, wake_hour=6, wake_minute=30, curfew_timezone="America/New_York"
        )
        assert format_curfew_window(player) == "10:00 PM - 6:30 AM America/New_York"

    def test_midnight_and_noon_edge_cases(self):
        player = _player(curfew_hour=0, curfew_minute=0, wake_hour=12, wake_minute=0)
        assert format_curfew_window(player) == f"12:00 AM - 12:00 PM {DEFAULT_CURFEW_TIMEZONE}"


class TestIsValidTimezone:
    def test_known_timezone(self):
        assert is_valid_timezone("America/New_York") is True

    def test_unknown_timezone(self):
        assert is_valid_timezone("Not/AZone") is False
