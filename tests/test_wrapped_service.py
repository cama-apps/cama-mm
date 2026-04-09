"""
Tests for Cama Wrapped yearly summary feature.
"""

import time
from datetime import UTC, datetime
from unittest.mock import MagicMock

from repositories.wrapped_repository import WrappedRepository
from services.wrapped_service import Award, WrappedService


class TestWrappedRepository:
    """Tests for WrappedRepository."""

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

    def test_get_year_timestamps(self, repo_db_path):
        """Test year timestamp calculation."""
        repo = WrappedRepository(repo_db_path)
        service = WrappedService(
            wrapped_repo=repo,
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
        )

        start_ts, end_ts = service._get_year_timestamps(2026)

        start_dt = datetime.fromtimestamp(start_ts, tz=UTC)
        end_dt = datetime.fromtimestamp(end_ts - 1, tz=UTC)

        assert start_dt.year == 2026
        assert start_dt.month == 1
        assert start_dt.day == 1

        assert end_dt.year == 2026
        assert end_dt.month == 12
        assert end_dt.day == 31

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
        from repositories.bet_repository import BetRepository
        from repositories.match_repository import MatchRepository
        from repositories.player_repository import PlayerRepository

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
        result = service.get_server_wrapped(0, 2026)
        assert result is None

    def test_get_player_wrapped_not_registered(self, repo_db_path):
        """Test getting player wrapped for non-existent player."""
        from repositories.bet_repository import BetRepository
        from repositories.match_repository import MatchRepository
        from repositories.player_repository import PlayerRepository

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
        result = service.get_player_wrapped(999999, 2026)
        assert result is None
