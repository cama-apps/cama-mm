"""Tests for utils/timezone.py: validation and the /player timezone list menu."""

from utils.timezone import (
    COMMON_TIMEZONES,
    DEFAULT_TIMEZONE,
    format_common_timezones,
    is_valid_timezone,
)


class TestIsValidTimezone:
    def test_known_timezone(self):
        assert is_valid_timezone("America/New_York") is True

    def test_unknown_timezone(self):
        assert is_valid_timezone("Not/AZone") is False


class TestCommonTimezones:
    def test_every_curated_zone_is_a_real_iana_name(self):
        """Guards against a typo in the curated menu silently pointing at a fake zone."""
        for zones in COMMON_TIMEZONES.values():
            for name, _label in zones:
                assert is_valid_timezone(name), f"{name} is not a valid IANA timezone"

    def test_default_timezone_is_in_the_menu(self):
        us_names = {name for name, _ in COMMON_TIMEZONES["US"]}
        assert DEFAULT_TIMEZONE in us_names

    def test_no_duplicate_zone_names_across_regions(self):
        all_names = [name for zones in COMMON_TIMEZONES.values() for name, _ in zones]
        assert len(all_names) == len(set(all_names))


class TestFormatCommonTimezones:
    def test_includes_every_curated_zone(self):
        message = format_common_timezones()
        for zones in COMMON_TIMEZONES.values():
            for name, _label in zones:
                assert f"`{name}`" in message

    def test_includes_every_region_heading(self):
        message = format_common_timezones()
        for region in COMMON_TIMEZONES:
            assert f"**{region}**" in message

    def test_notes_any_iana_name_is_accepted(self):
        assert "any" in format_common_timezones().lower()

    def test_fits_within_discord_message_limit(self):
        """Discord caps plain message content at 2000 characters."""
        assert len(format_common_timezones()) <= 2000
