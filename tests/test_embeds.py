"""
Tests for embed utilities.
"""

from datetime import datetime
from unittest.mock import MagicMock

from utils.embeds import (
    _format_duration,
    _format_number,
    create_enriched_match_embed,
    create_lobby_embed,
    create_match_summary_embed,
)


class TestLobbyEmbedTimestamp:
    """Test that lobby embed uses Discord dynamic timestamps."""

    def test_lobby_embed_uses_discord_timestamp_format(self):
        """Verify lobby description uses <t:TIMESTAMP:t> format for user-local time."""
        # Create a lobby with a known created_at time
        lobby = MagicMock()
        lobby.created_at = datetime(2026, 1, 2, 12, 30, 0)
        lobby.get_player_count.return_value = 5

        embed = create_lobby_embed(lobby, players=[], player_ids={}, ready_threshold=10)

        expected_ts = int(lobby.created_at.timestamp())

        assert f"<t:{expected_ts}:t>" in embed.description
        assert "Opened at" in embed.description

    def test_lobby_embed_no_created_at_fallback(self):
        """Verify fallback when lobby has no created_at."""
        lobby = MagicMock()
        lobby.created_at = None
        lobby.get_player_count.return_value = 0

        embed = create_lobby_embed(lobby, players=[], player_ids={}, ready_threshold=10)

        assert "Opened just now" in embed.description

    def test_lobby_embed_timestamp_is_unix_epoch(self):
        """Verify the timestamp is a valid Unix epoch integer."""
        lobby = MagicMock()
        lobby.created_at = datetime.now()
        lobby.get_player_count.return_value = 3

        embed = create_lobby_embed(lobby, players=[], player_ids={}, ready_threshold=10)

        # Extract timestamp from <t:TIMESTAMP:t>
        import re

        match = re.search(r"<t:(\d+):t>", embed.description)
        assert match is not None, f"No Discord timestamp found in: {embed.description}"

        timestamp = int(match.group(1))
        # Verify it's a reasonable Unix timestamp (after year 2020)
        assert timestamp > 1577836800, "Timestamp should be after 2020"


class TestFormatHelpers:
    """Tests for embed formatting helper functions."""

    def test_format_number_small(self):
        """Small numbers should display as-is."""
        assert _format_number(500) == "500"
        assert _format_number(0) == "0"
        assert _format_number(999) == "999"

    def test_format_number_large(self):
        """Large numbers should display with k suffix."""
        assert _format_number(1000) == "1.0k"
        assert _format_number(45123) == "45.1k"
        assert _format_number(100000) == "100.0k"

    def test_format_number_none(self):
        """None should display as em dash."""
        assert _format_number(None) == "—"

    def test_format_duration_valid(self):
        """Valid durations should format as MM:SS."""
        assert _format_duration(1985) == "33:05"
        assert _format_duration(3600) == "60:00"
        assert _format_duration(90) == "1:30"
        assert _format_duration(59) == "0:59"

    def test_format_duration_invalid(self):
        """Invalid durations should return em dash."""
        assert _format_duration(None) == "—"
        assert _format_duration(0) == "—"
        assert _format_duration(-100) == "—"


class TestEnrichedMatchEmbed:
    """Tests for create_enriched_match_embed."""

    def test_creates_embed_with_radiant_victory(self):
        """Embed should show Radiant victory correctly."""
        radiant = [
            {
                "hero_id": 1,
                "kills": 10,
                "deaths": 2,
                "assists": 5,
                "hero_damage": 25000,
                "net_worth": 18000,
            },
        ]
        dire = [
            {
                "hero_id": 2,
                "kills": 3,
                "deaths": 8,
                "assists": 2,
                "hero_damage": 12000,
                "net_worth": 10000,
            },
        ]

        embed = create_enriched_match_embed(
            match_id=123,
            valve_match_id=7500000000,
            duration_seconds=1985,
            radiant_score=45,
            dire_score=32,
            winning_team=1,
            radiant_participants=radiant,
            dire_participants=dire,
        )

        assert "Match #123" in embed.title
        assert "Radiant Victory" in embed.title
        assert "33:05" in embed.title
        assert "45 - 32" in embed.description
        # Check links
        assert "opendota.com/matches/7500000000" in embed.description
        assert "dotabuff.com/matches/7500000000" in embed.description

    def test_creates_embed_with_dire_victory(self):
        """Embed should show Dire victory correctly."""
        embed = create_enriched_match_embed(
            match_id=456,
            valve_match_id=None,
            duration_seconds=None,
            radiant_score=None,
            dire_score=None,
            winning_team=2,
            radiant_participants=[],
            dire_participants=[],
        )

        assert "Dire Victory" in embed.title

    def test_embed_has_team_fields(self):
        """Embed should have fields for both teams."""
        embed = create_enriched_match_embed(
            match_id=789,
            valve_match_id=123,
            duration_seconds=1800,
            radiant_score=30,
            dire_score=25,
            winning_team=1,
            radiant_participants=[{"hero_id": 1, "kills": 5, "deaths": 3, "assists": 10}],
            dire_participants=[{"hero_id": 2, "kills": 4, "deaths": 5, "assists": 8}],
        )

        field_names = [f.name for f in embed.fields]
        assert any("RADIANT" in name for name in field_names)
        assert any("DIRE" in name for name in field_names)
        assert any("Winner" in name for name in field_names)


class TestLaneOutcomeDisplay:
    """Tests for lane won/lost display in embeds."""

    def test_lane_won_lost_shown_in_embed(self):
        """Lane W/L/D should be shown based on efficiency comparison."""
        # Radiant mid (72%) vs Dire mid (55%) - Radiant wins mid
        radiant = [
            {
                "hero_id": 1,
                "kills": 10,
                "deaths": 2,
                "assists": 5,
                "hero_damage": 25000,
                "net_worth": 18000,
                "lane_role": 2,  # Mid
                "lane_efficiency": 72,
            },
        ]
        dire = [
            {
                "hero_id": 2,
                "kills": 3,
                "deaths": 8,
                "assists": 2,
                "hero_damage": 12000,
                "net_worth": 10000,
                "lane_role": 2,  # Mid
                "lane_efficiency": 55,
            },
        ]

        embed = create_enriched_match_embed(
            match_id=123,
            valve_match_id=7500000000,
            duration_seconds=1985,
            radiant_score=45,
            dire_score=32,
            winning_team=1,
            radiant_participants=radiant,
            dire_participants=dire,
        )

        # Find the team fields
        radiant_field = None
        dire_field = None
        for f in embed.fields:
            if "RADIANT" in f.name:
                radiant_field = f
            elif "DIRE" in f.name:
                dire_field = f

        assert radiant_field is not None
        assert dire_field is not None

        # Radiant mid should show W (won lane)
        assert "Mid W" in radiant_field.value
        # Dire mid should show L (lost lane)
        assert "Mid L" in dire_field.value

    def test_lane_draw_when_close_efficiency(self):
        """Lane should show D when efficiencies are within 5%."""
        radiant = [
            {
                "hero_id": 1,
                "kills": 5,
                "deaths": 3,
                "assists": 8,
                "hero_damage": 20000,
                "net_worth": 15000,
                "lane_role": 2,  # Mid
                "lane_efficiency": 65,
            },
        ]
        dire = [
            {
                "hero_id": 2,
                "kills": 4,
                "deaths": 4,
                "assists": 6,
                "hero_damage": 18000,
                "net_worth": 14000,
                "lane_role": 2,  # Mid
                "lane_efficiency": 63,  # Within 5% of 65
            },
        ]

        embed = create_enriched_match_embed(
            match_id=456,
            valve_match_id=None,
            duration_seconds=1800,
            radiant_score=30,
            dire_score=25,
            winning_team=1,
            radiant_participants=radiant,
            dire_participants=dire,
        )

        radiant_field = [f for f in embed.fields if "RADIANT" in f.name][0]
        dire_field = [f for f in embed.fields if "DIRE" in f.name][0]

        # Both should show D (draw/even)
        assert "Mid D" in radiant_field.value
        assert "Mid D" in dire_field.value

    def test_side_lanes_compare_correctly(self):
        """Safe lane vs Off lane should compare correctly."""
        # Radiant safe (75%) vs Dire off (50%) - Radiant wins their safe lane
        radiant = [
            {
                "hero_id": 1,
                "kills": 8,
                "deaths": 1,
                "assists": 3,
                "hero_damage": 30000,
                "net_worth": 20000,
                "lane_role": 1,  # Safe
                "lane_efficiency": 75,
            },
        ]
        dire = [
            {
                "hero_id": 2,
                "kills": 2,
                "deaths": 5,
                "assists": 4,
                "hero_damage": 15000,
                "net_worth": 12000,
                "lane_role": 3,  # Off (opposes Radiant Safe)
                "lane_efficiency": 50,
            },
        ]

        embed = create_enriched_match_embed(
            match_id=789,
            valve_match_id=None,
            duration_seconds=2000,
            radiant_score=35,
            dire_score=28,
            winning_team=1,
            radiant_participants=radiant,
            dire_participants=dire,
        )

        radiant_field = [f for f in embed.fields if "RADIANT" in f.name][0]
        dire_field = [f for f in embed.fields if "DIRE" in f.name][0]

        # Radiant safe won, Dire off lost
        assert "Safe W" in radiant_field.value
        assert "Off L" in dire_field.value

    def test_lane_role_without_efficiency(self):
        """Lane role should show just lane name when no efficiency data."""
        radiant = [
            {
                "hero_id": 1,
                "kills": 5,
                "deaths": 2,
                "assists": 3,
                "hero_damage": 20000,
                "net_worth": 15000,
                "lane_role": 3,  # Off lane
                "lane_efficiency": None,
            },
        ]

        embed = create_enriched_match_embed(
            match_id=456,
            valve_match_id=None,
            duration_seconds=1800,
            radiant_score=30,
            dire_score=25,
            winning_team=1,
            radiant_participants=radiant,
            dire_participants=[],
        )

        radiant_field = [f for f in embed.fields if "RADIANT" in f.name][0]
        # Should show just "Off" without W/L/D
        assert "Off" in radiant_field.value
        assert "Off W" not in radiant_field.value
        assert "Off L" not in radiant_field.value


class TestMatchSummaryEmbed:
    """Tests for create_match_summary_embed."""

    def test_creates_simple_embed(self):
        """Summary embed should show basic info."""
        radiant = [{"hero_id": 1, "kills": 5, "deaths": 3, "assists": 10}]
        dire = [{"hero_id": 2, "kills": 4, "deaths": 5, "assists": 8}]

        embed = create_match_summary_embed(
            match_id=100,
            winning_team=1,
            radiant_participants=radiant,
            dire_participants=dire,
            valve_match_id=123456,
        )

        assert "Match #100" in embed.title
        assert "Radiant Victory" in embed.title

    def test_shows_links_when_valve_match_id_present(self):
        """Summary embed should include links when valve_match_id is present."""
        embed = create_match_summary_embed(
            match_id=100,
            winning_team=2,
            radiant_participants=[],
            dire_participants=[],
            valve_match_id=9999999,
        )

        # Should have a Links field
        link_fields = [f for f in embed.fields if f.name == "Links"]
        assert len(link_fields) == 1
        assert "9999999" in link_fields[0].value
