"""
Tests for Cama Wrapped monthly summary feature.
"""

import json
import time
from datetime import datetime
from unittest.mock import MagicMock

import pytest

from repositories.wrapped_repository import WrappedRepository
from services.wrapped_service import Award, WrappedService


class TestWrappedRepository:
    """Tests for WrappedRepository."""

    def test_save_and_get_wrapped(self, repo_db_path):
        """Test saving and retrieving wrapped generation record."""
        repo = WrappedRepository(repo_db_path)

        year_month = "2026-01"
        stats = {"total_matches": 47, "unique_players": 15}

        # Save wrapped
        record_id = repo.save_wrapped(
            guild_id=123,
            year_month=year_month,
            stats=stats,
            channel_id=456,
            message_id=789,
            generated_by=111,
            generation_type="manual",
        )

        assert record_id > 0

        # Retrieve wrapped
        result = repo.get_wrapped(123, year_month)
        assert result is not None
        assert result["year_month"] == year_month
        assert result["channel_id"] == 456
        assert result["message_id"] == 789
        assert result["generated_by"] == 111
        assert result["generation_type"] == "manual"

        # Verify stats JSON
        parsed_stats = json.loads(result["stats_json"])
        assert parsed_stats["total_matches"] == 47

    def test_get_wrapped_not_found(self, repo_db_path):
        """Test getting non-existent wrapped returns None."""
        repo = WrappedRepository(repo_db_path)
        result = repo.get_wrapped(123, "2020-01")
        assert result is None

    def test_save_wrapped_upsert(self, repo_db_path):
        """Test that saving wrapped for same guild/month updates existing record."""
        repo = WrappedRepository(repo_db_path)

        year_month = "2026-01"

        # First save
        repo.save_wrapped(
            guild_id=123,
            year_month=year_month,
            stats={"version": 1},
        )

        # Second save (update)
        repo.save_wrapped(
            guild_id=123,
            year_month=year_month,
            stats={"version": 2},
            generation_type="auto",
        )

        # Should only have one record with updated stats
        result = repo.get_wrapped(123, year_month)
        parsed = json.loads(result["stats_json"])
        assert parsed["version"] == 2

    def test_get_month_summary_empty(self, repo_db_path):
        """Test getting month summary with no matches."""
        repo = WrappedRepository(repo_db_path)

        now = int(time.time())
        start_ts = now - 86400 * 30  # 30 days ago
        end_ts = now

        summary = repo.get_month_summary(0, start_ts, end_ts)
        # Should return empty or zeros
        assert summary.get("total_matches", 0) == 0


class TestWrappedService:
    """Tests for WrappedService."""

    def test_get_month_timestamps(self, repo_db_path):
        """Test month timestamp calculation."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        start_ts, end_ts = service._get_month_timestamps("2026-01")

        # January 2026
        start_dt = datetime.fromtimestamp(start_ts)
        end_dt = datetime.fromtimestamp(end_ts - 1)  # -1 to get last second of month

        assert start_dt.year == 2026
        assert start_dt.month == 1
        assert start_dt.day == 1

        assert end_dt.year == 2026
        assert end_dt.month == 1
        assert end_dt.day == 31

    def test_get_month_timestamps_february(self, repo_db_path):
        """Test month timestamp for February (shorter month)."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        start_ts, end_ts = service._get_month_timestamps("2026-02")

        end_dt = datetime.fromtimestamp(end_ts - 1)
        assert end_dt.month == 2
        assert end_dt.day == 28  # 2026 is not a leap year

    def test_was_wrapped_generated(self, repo_db_path):
        """Test checking if wrapped was generated."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        # Not generated yet
        assert service.was_wrapped_generated(123, "2026-01") is False

        # Mark as generated
        service.mark_wrapped_generated(
            guild_id=123,
            year_month="2026-01",
            stats={"test": True},
        )

        # Now should be generated
        assert service.was_wrapped_generated(123, "2026-01") is True

    def test_get_cached_wrapped(self, repo_db_path):
        """Test retrieving cached wrapped stats."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        # No cache yet
        assert service.get_cached_wrapped(123, "2026-01") is None

        # Save with stats
        service.mark_wrapped_generated(
            guild_id=123,
            year_month="2026-01",
            stats={"total_matches": 50, "awards": []},
        )

        # Get cached
        cached = service.get_cached_wrapped(123, "2026-01")
        assert cached is not None
        assert cached["total_matches"] == 50

    def test_can_generate_wrapped_month_not_complete(self, repo_db_path):
        """Test that wrapped cannot be generated for incomplete months."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        # Try to generate for current month (should fail)
        now = datetime.now()
        current_month = now.strftime("%Y-%m")
        can_gen, reason = service.can_generate_wrapped(123, current_month)
        assert can_gen is False
        assert "not yet complete" in reason

        # Try to generate for a future month (should fail)
        can_gen, reason = service.can_generate_wrapped(123, "2030-01")
        assert can_gen is False
        assert "not yet complete" in reason

    def test_can_generate_wrapped_already_generated(self, repo_db_path):
        """Test that wrapped cannot be regenerated for same month."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        # Mark 2026-01 as generated
        service.mark_wrapped_generated(
            guild_id=123,
            year_month="2026-01",
            stats={"test": True},
        )

        # Try to generate again (should fail)
        can_gen, reason = service.can_generate_wrapped(123, "2026-01")
        assert can_gen is False
        assert "already generated" in reason

    def test_can_generate_wrapped_cooldown(self, repo_db_path):
        """Test that wrapped requires 25 days between generations."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        # Mark 2025-12 as generated recently
        service.mark_wrapped_generated(
            guild_id=123,
            year_month="2025-12",
            stats={"test": True},
        )

        # Try to generate 2026-01 immediately (should fail due to cooldown)
        can_gen, reason = service.can_generate_wrapped(123, "2026-01")
        assert can_gen is False
        assert "days since last" in reason

    def test_can_generate_wrapped_success(self, repo_db_path):
        """Test that wrapped can be generated for completed month with no cooldown."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        # No previous generation, completed month - should succeed
        can_gen, reason = service.can_generate_wrapped(123, "2026-01")
        assert can_gen is True
        assert reason == "OK"

    def test_generate_awards_empty_data(self, repo_db_path):
        """Test award generation with no data."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        awards = service._generate_awards(
            match_stats=[],
            hero_stats=[],
            player_heroes=[],
            rating_changes=[],
            betting_stats=[],
            bets_against=[],
            bankruptcies=[],
        )

        assert awards == []

    def test_generate_awards_with_data(self, repo_db_path):
        """Test award generation with sample data."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        # Sample match stats
        match_stats = [
            {
                "discord_id": i,
                "discord_username": f"Player{i}",
                "games_played": 10 + i,
                "wins": 5 + i % 3,
                "losses": 5,
                "avg_gpm": 400 + i * 20,
                "avg_xpm": 450 + i * 15,
                "avg_kda": 2.0 + i * 0.5,
                "total_kills": 50,
                "total_deaths": 25,
                "total_assists": 60,
                "total_wards": 10 * i,
                "total_fantasy": 100,
                "glicko_rating": 1500 + i * 50,
                "glicko_rd": 50,
            }
            for i in range(1, 13)
        ]

        # Sample rating changes
        rating_changes = [
            {
                "discord_id": 1,
                "discord_username": "Player1",
                "first_rating": 1500,
                "last_rating": 1650,
                "rating_change": 150,
                "rating_variance": 100,
            },
            {
                "discord_id": 2,
                "discord_username": "Player2",
                "first_rating": 1600,
                "last_rating": 1400,
                "rating_change": -200,
                "rating_variance": 2500,
            },
        ]

        awards = service._generate_awards(
            match_stats=match_stats,
            hero_stats=[],
            player_heroes=[],
            rating_changes=rating_changes,
            betting_stats=[],
            bets_against=[],
            bankruptcies=[],
        )

        # Should have generated some awards
        assert len(awards) > 0

        # Check for expected award types
        award_titles = [a.title for a in awards]

        # Should have performance awards (Gold Goblin for GPM)
        assert "Gold Goblin" in award_titles

        # Should have rating awards
        assert "Elo Inflation" in award_titles
        assert "The Cliff" in award_titles

        # Should have fun awards (Iron Man for most games)
        assert "No Life" in award_titles

    def test_award_dataclass(self):
        """Test Award dataclass creation."""
        award = Award(
            category="performance",
            title="Gold Goblin",
            stat_name="Best GPM",
            stat_value="847 avg",
            discord_id=123,
            discord_username="TestUser",
            emoji="💰",
            flavor_text="Farming simulator champion",
        )

        assert award.category == "performance"
        assert award.title == "Gold Goblin"
        assert award.emoji == "💰"


class TestWrappedServiceIntegration:
    """Integration tests with actual database."""

    def test_get_server_wrapped_no_data(self, repo_db_path):
        """Test getting server wrapped with no match data."""
        from repositories.player_repository import PlayerRepository
        from repositories.match_repository import MatchRepository
        from repositories.bet_repository import BetRepository

        wrapped_repo = WrappedRepository(repo_db_path)
        player_repo = PlayerRepository(repo_db_path)
        match_repo = MatchRepository(repo_db_path)
        bet_repo = BetRepository(repo_db_path)

        service = WrappedService(
            wrapped_repo=wrapped_repo,
            player_repo=player_repo,
            match_repo=match_repo,
            bet_repo=bet_repo,
        )

        # Should return None with no data
        result = service.get_server_wrapped(0, "2026-01")
        assert result is None

    def test_get_player_wrapped_not_registered(self, repo_db_path):
        """Test getting player wrapped for non-existent player."""
        from repositories.player_repository import PlayerRepository
        from repositories.match_repository import MatchRepository
        from repositories.bet_repository import BetRepository

        wrapped_repo = WrappedRepository(repo_db_path)
        player_repo = PlayerRepository(repo_db_path)
        match_repo = MatchRepository(repo_db_path)
        bet_repo = BetRepository(repo_db_path)

        service = WrappedService(
            wrapped_repo=wrapped_repo,
            player_repo=player_repo,
            match_repo=match_repo,
            bet_repo=bet_repo,
        )

        # Should return None for non-existent player
        result = service.get_player_wrapped(999999, "2026-01")
        assert result is None
