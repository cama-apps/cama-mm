"""
Tests for the /leaderboard command type parameter routing.

Tests that the type parameter correctly routes to:
- balance (default)
- gambling
- predictions
"""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch
from dataclasses import dataclass

from database import Database
from repositories.player_repository import PlayerRepository


@pytest.fixture
def temp_db_path(tmp_path):
    """Create a temporary database path for testing."""
    db_path = str(tmp_path / "test_leaderboard.db")
    Database(db_path)
    return db_path


@pytest.fixture
def player_repo(temp_db_path):
    """Create a PlayerRepository instance."""
    return PlayerRepository(temp_db_path)


@pytest.fixture
def mock_rate_limiter():
    """Patch the rate limiter to always allow requests."""
    mock_result = MagicMock()
    mock_result.allowed = True
    with patch("commands.info.GLOBAL_RATE_LIMITER.check", return_value=mock_result):
        yield


@pytest.fixture
def mock_discord_helpers():
    """Patch safe_defer and safe_followup."""
    with patch("commands.info.safe_defer", new_callable=AsyncMock) as mock_defer:
        with patch("commands.info.safe_followup", new_callable=AsyncMock) as mock_followup:
            mock_defer.return_value = True
            mock_followup.return_value = MagicMock()
            yield {"defer": mock_defer, "followup": mock_followup}


def register_players(player_repo, player_ids):
    """Helper to register test players with varied balances."""
    for i, pid in enumerate(player_ids):
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=3000,
        )
        # Set varied balances for leaderboard testing
        player_repo.update_balance(pid, (len(player_ids) - i) * 10)


class MockInteraction:
    """Mock Discord interaction for testing."""

    def __init__(self, user_id: int = 1001, guild_id: int = 12345):
        self.user = MagicMock()
        self.user.id = user_id
        self.guild = MagicMock()
        self.guild.id = guild_id
        self.guild.get_member = MagicMock(return_value=None)
        self.response = MagicMock()
        self.response.is_done = MagicMock(return_value=False)
        self.response.send_message = AsyncMock()
        self.response.defer = AsyncMock()
        self.followup = MagicMock()
        self.followup.send = AsyncMock()


class MockChoice:
    """Mock Discord Choice object."""

    def __init__(self, value: str):
        self.value = value


@dataclass
class MockGamblingLeaderboard:
    """Mock gambling leaderboard result."""

    top_earners: list
    down_bad: list
    hall_of_degen: list
    biggest_gamblers: list
    server_stats: dict


class TestLeaderboardTypeRouting:
    """Tests for leaderboard type parameter routing."""

    @pytest.fixture
    def info_cog(self, player_repo):
        """Create an InfoCommands cog with mocked services."""
        from commands.info import InfoCommands

        mock_bot = MagicMock()
        mock_match_repo = MagicMock()

        # Mock gambling stats service
        mock_gambling_service = MagicMock()
        mock_gambling_service.get_leaderboard = MagicMock(
            return_value=MockGamblingLeaderboard(
                top_earners=[{"discord_id": 1, "net_pnl": 100, "win_rate": 0.6}],
                down_bad=[],
                hall_of_degen=[{"discord_id": 2, "degen_score": 50, "degen_emoji": "🎰", "degen_title": "Degen"}],
                biggest_gamblers=[{"discord_id": 1, "total_bets": 10, "total_wagered": 100}],
                server_stats={"total_bets": 50, "total_wagered": 500, "unique_gamblers": 5},
            )
        )

        # Mock prediction service
        mock_prediction_service = MagicMock()
        mock_prediction_service.prediction_repo = MagicMock()
        mock_prediction_service.prediction_repo.get_prediction_leaderboard = MagicMock(
            return_value={
                "top_earners": [{"discord_id": 1, "net_pnl": 50, "win_rate": 0.7, "wins": 7, "losses": 3}],
                "down_bad": [],
                "most_accurate": [{"discord_id": 1, "win_rate": 0.7, "wins": 7, "losses": 3}],
            }
        )
        mock_prediction_service.prediction_repo.get_server_prediction_stats = MagicMock(
            return_value={"total_predictions": 10, "total_bets": 30, "total_wagered": 300}
        )

        return InfoCommands(
            bot=mock_bot,
            player_repo=player_repo,
            match_repo=mock_match_repo,
            role_emojis={},
            role_names={},
            gambling_stats_service=mock_gambling_service,
            prediction_service=mock_prediction_service,
            bankruptcy_service=None,
        )

    @pytest.mark.asyncio
    async def test_leaderboard_default_is_balance(
        self, info_cog, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that leaderboard with no type defaults to balance."""
        register_players(player_repo, [1, 2, 3, 4, 5])
        interaction = MockInteraction()

        await info_cog.leaderboard.callback(info_cog, interaction, type=None, limit=20)

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "embed" in call_kwargs
        assert "Leaderboard" in call_kwargs["embed"].title

    @pytest.mark.asyncio
    async def test_leaderboard_type_balance(
        self, info_cog, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that type=balance routes to balance leaderboard."""
        register_players(player_repo, [1, 2, 3, 4, 5])
        interaction = MockInteraction()

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("balance"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "embed" in call_kwargs
        assert "Leaderboard" in call_kwargs["embed"].title

    @pytest.mark.asyncio
    async def test_leaderboard_type_gambling_routes_correctly(
        self, info_cog, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that type=gambling routes to gambling leaderboard."""
        interaction = MockInteraction()

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("gambling"), limit=20
        )

        # Verify gambling_stats_service.get_leaderboard was called
        info_cog.gambling_stats_service.get_leaderboard.assert_called_once()

        mock_followup = mock_discord_helpers["followup"]
        assert mock_followup.called
        call_kwargs = mock_followup.call_args.kwargs
        assert "embed" in call_kwargs
        assert "GAMBLING" in call_kwargs["embed"].title.upper()

    @pytest.mark.asyncio
    async def test_leaderboard_type_predictions_routes_correctly(
        self, info_cog, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that type=predictions routes to predictions leaderboard."""
        interaction = MockInteraction()

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("predictions"), limit=20
        )

        # Verify prediction_service.prediction_repo.get_prediction_leaderboard was called
        info_cog.prediction_service.prediction_repo.get_prediction_leaderboard.assert_called_once()

        mock_followup = mock_discord_helpers["followup"]
        assert mock_followup.called
        call_kwargs = mock_followup.call_args.kwargs
        assert "embed" in call_kwargs
        assert "PREDICTION" in call_kwargs["embed"].title.upper()

    @pytest.mark.asyncio
    async def test_gambling_leaderboard_unavailable_service(
        self, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Test graceful handling when gambling service unavailable."""
        from commands.info import InfoCommands

        info_cog = InfoCommands(
            bot=MagicMock(),
            player_repo=player_repo,
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
            gambling_stats_service=None,
            prediction_service=None,
        )

        interaction = MockInteraction()

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("gambling"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "content" in call_kwargs
        assert "not available" in call_kwargs["content"].lower()

    @pytest.mark.asyncio
    async def test_predictions_leaderboard_unavailable_service(
        self, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Test graceful handling when prediction service unavailable."""
        from commands.info import InfoCommands

        info_cog = InfoCommands(
            bot=MagicMock(),
            player_repo=player_repo,
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
            gambling_stats_service=None,
            prediction_service=None,
        )

        interaction = MockInteraction()

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("predictions"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "content" in call_kwargs
        assert "not available" in call_kwargs["content"].lower()


class TestLeaderboardLimitParameter:
    """Tests for the limit parameter validation."""

    @pytest.fixture
    def info_cog(self, player_repo):
        """Create an InfoCommands cog."""
        from commands.info import InfoCommands

        return InfoCommands(
            bot=MagicMock(),
            player_repo=player_repo,
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
        )

    @pytest.mark.asyncio
    async def test_leaderboard_invalid_limit_rejected(
        self, info_cog, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that invalid limits are rejected."""
        interaction = MockInteraction()

        await info_cog.leaderboard.callback(info_cog, interaction, type=None, limit=150)

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "content" in call_kwargs
        assert "between 1 and 100" in call_kwargs["content"].lower()

    @pytest.mark.asyncio
    async def test_leaderboard_limit_zero_rejected(
        self, info_cog, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that limit=0 is rejected."""
        interaction = MockInteraction()

        await info_cog.leaderboard.callback(info_cog, interaction, type=None, limit=0)

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "content" in call_kwargs
        assert "between 1 and 100" in call_kwargs["content"].lower()


class TestLeaderboardGamblingContent:
    """Tests for gambling leaderboard content."""

    @pytest.fixture
    def info_cog_with_gambling(self, player_repo):
        """Create InfoCommands with gambling service that returns specific data."""
        from commands.info import InfoCommands

        mock_gambling_service = MagicMock()
        mock_gambling_service.get_leaderboard = MagicMock(
            return_value=MockGamblingLeaderboard(
                top_earners=[
                    {"discord_id": 1001, "net_pnl": 500, "win_rate": 0.75},
                    {"discord_id": 1002, "net_pnl": 200, "win_rate": 0.60},
                ],
                down_bad=[
                    {"discord_id": 1003, "net_pnl": -300, "win_rate": 0.30},
                ],
                hall_of_degen=[
                    {"discord_id": 1004, "degen_score": 85, "degen_emoji": "🔥", "degen_title": "Mega Degen"},
                ],
                biggest_gamblers=[
                    {"discord_id": 1001, "total_bets": 50, "total_wagered": 1000},
                ],
                server_stats={"total_bets": 100, "total_wagered": 2000, "unique_gamblers": 10},
            )
        )

        return InfoCommands(
            bot=MagicMock(),
            player_repo=player_repo,
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
            gambling_stats_service=mock_gambling_service,
        )

    @pytest.mark.asyncio
    async def test_gambling_leaderboard_has_all_sections(
        self, info_cog_with_gambling, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that gambling leaderboard contains all expected sections."""
        interaction = MockInteraction()

        await info_cog_with_gambling.leaderboard.callback(
            info_cog_with_gambling, interaction, type=MockChoice("gambling"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        call_kwargs = mock_followup.call_args.kwargs
        embed = call_kwargs["embed"]

        field_names = [f.name for f in embed.fields]

        # Should have top earners
        assert any("Top Earners" in name for name in field_names)
        # Should have down bad (since we have negative entries)
        assert any("Down Bad" in name for name in field_names)
        # Should have hall of degen
        assert any("Hall of Degen" in name for name in field_names)
        # Should have biggest gamblers
        assert any("Biggest Gamblers" in name for name in field_names)


class TestLeaderboardPredictionsContent:
    """Tests for predictions leaderboard content."""

    @pytest.fixture
    def info_cog_with_predictions(self, player_repo):
        """Create InfoCommands with prediction service that returns specific data."""
        from commands.info import InfoCommands

        mock_prediction_service = MagicMock()
        mock_prediction_service.prediction_repo = MagicMock()
        mock_prediction_service.prediction_repo.get_prediction_leaderboard = MagicMock(
            return_value={
                "top_earners": [
                    {"discord_id": 1001, "net_pnl": 200, "win_rate": 0.80, "wins": 8, "losses": 2},
                ],
                "down_bad": [
                    {"discord_id": 1002, "net_pnl": -100, "win_rate": 0.40, "wins": 4, "losses": 6},
                ],
                "most_accurate": [
                    {"discord_id": 1001, "win_rate": 0.80, "wins": 8, "losses": 2},
                ],
            }
        )
        mock_prediction_service.prediction_repo.get_server_prediction_stats = MagicMock(
            return_value={"total_predictions": 20, "total_bets": 50, "total_wagered": 500}
        )

        return InfoCommands(
            bot=MagicMock(),
            player_repo=player_repo,
            match_repo=MagicMock(),
            role_emojis={},
            role_names={},
            prediction_service=mock_prediction_service,
        )

    @pytest.mark.asyncio
    async def test_predictions_leaderboard_has_all_sections(
        self, info_cog_with_predictions, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that predictions leaderboard contains all expected sections."""
        interaction = MockInteraction()

        await info_cog_with_predictions.leaderboard.callback(
            info_cog_with_predictions, interaction, type=MockChoice("predictions"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        call_kwargs = mock_followup.call_args.kwargs
        embed = call_kwargs["embed"]

        field_names = [f.name for f in embed.fields]

        # Should have top earners
        assert any("Top Earners" in name for name in field_names)
        # Should have most accurate
        assert any("Most Accurate" in name for name in field_names)
