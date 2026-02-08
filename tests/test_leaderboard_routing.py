"""
Tests for the /leaderboard command type parameter routing.

Tests that the type parameter correctly routes to:
- balance (default)
- gambling
- predictions
"""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch

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


TEST_GUILD_ID = 12345


def register_players(player_repo, player_ids, guild_id=TEST_GUILD_ID):
    """Helper to register test players with varied balances."""
    for i, pid in enumerate(player_ids):
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=guild_id,
            initial_mmr=3000,
        )
        # Set varied balances for leaderboard testing
        player_repo.update_balance(pid, guild_id, (len(player_ids) - i) * 10)


class MockInteraction:
    """Mock Discord interaction for testing."""

    def __init__(self, user_id: int = 1001, guild_id: int = 12345):
        self.user = MagicMock()
        self.user.id = user_id
        self.guild = MagicMock()
        self.guild.id = guild_id
        self.guild.get_member = MagicMock(return_value=None)
        self.guild.members = []  # Required for UnifiedLeaderboardView
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


class TestLeaderboardTypeRouting:
    """Tests for leaderboard type parameter routing."""

    @pytest.fixture
    def info_cog(self, player_repo):
        """Create an InfoCommands cog with mocked services."""
        from commands.info import InfoCommands
        from services.gambling_stats_service import Leaderboard, LeaderboardEntry, ServerStats

        mock_bot = MagicMock()
        mock_match_repo = MagicMock()

        # Use real Leaderboard and LeaderboardEntry dataclasses
        server_stats: ServerStats = {
            "total_bets": 50,
            "total_wagered": 500,
            "unique_gamblers": 5,
            "avg_bet_size": 10,
            "total_bankruptcies": 0,
        }
        mock_gambling_service = MagicMock()
        mock_gambling_service.get_leaderboard = MagicMock(
            return_value=Leaderboard(
                top_earners=[LeaderboardEntry(discord_id=1, total_bets=10, wins=6, losses=4, win_rate=0.6, net_pnl=100, total_wagered=100, avg_leverage=1.5)],
                down_bad=[],
                hall_of_degen=[LeaderboardEntry(discord_id=2, total_bets=5, wins=2, losses=3, win_rate=0.4, net_pnl=-50, total_wagered=50, avg_leverage=2.0, degen_score=50, degen_emoji="🎰", degen_title="Degen")],
                biggest_gamblers=[LeaderboardEntry(discord_id=1, total_bets=10, wins=6, losses=4, win_rate=0.6, net_pnl=100, total_wagered=100, avg_leverage=1.5)],
                total_wagered=500,
                total_bets=50,
                avg_degen_score=40.0,
                total_bankruptcies=0,
                total_loans=0,
                server_stats=server_stats,
            )
        )

        # Mock prediction service
        mock_prediction_service = MagicMock()
        mock_prediction_service.get_prediction_leaderboard = MagicMock(
            return_value={
                "top_earners": [{"discord_id": 1, "net_pnl": 50, "win_rate": 0.7, "wins": 7, "losses": 3}],
                "down_bad": [],
                "most_accurate": [{"discord_id": 1, "win_rate": 0.7, "wins": 7, "losses": 3}],
            }
        )
        mock_prediction_service.get_server_prediction_stats = MagicMock(
            return_value={"total_predictions": 10, "total_bets": 30, "total_wagered": 300}
        )

        return InfoCommands(
            bot=mock_bot,
            player_service=player_repo,
            match_service=mock_match_repo,
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
        interaction.guild.members = []  # Add members list for unified view

        await info_cog.leaderboard.callback(info_cog, interaction, type=None, limit=20)

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "embed" in call_kwargs
        # Unified view uses "LEADERBOARD > Balance" format
        assert "LEADERBOARD" in call_kwargs["embed"].title.upper()

    @pytest.mark.asyncio
    async def test_leaderboard_type_balance(
        self, info_cog, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that type=balance routes to balance leaderboard."""
        register_players(player_repo, [1, 2, 3, 4, 5])
        interaction = MockInteraction()
        interaction.guild.members = []  # Add members list for unified view

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("balance"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        assert "embed" in call_kwargs
        # Unified view uses "LEADERBOARD > Balance" format
        assert "LEADERBOARD" in call_kwargs["embed"].title.upper()

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

        # Verify prediction_service.get_prediction_leaderboard was called
        info_cog.prediction_service.get_prediction_leaderboard.assert_called_once()

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
            player_service=player_repo,
            match_service=MagicMock(),
            role_emojis={},
            role_names={},
            gambling_stats_service=None,
            prediction_service=None,
        )

        interaction = MockInteraction()
        interaction.guild.members = []  # Add members list for unified view

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("gambling"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        # Unified view uses embed.description for "not available" message
        assert "embed" in call_kwargs
        assert "not available" in call_kwargs["embed"].description.lower()

    @pytest.mark.asyncio
    async def test_predictions_leaderboard_unavailable_service(
        self, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Test graceful handling when prediction service unavailable."""
        from commands.info import InfoCommands

        info_cog = InfoCommands(
            bot=MagicMock(),
            player_service=player_repo,
            match_service=MagicMock(),
            role_emojis={},
            role_names={},
            gambling_stats_service=None,
            prediction_service=None,
        )

        interaction = MockInteraction()
        interaction.guild.members = []  # Add members list for unified view

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("predictions"), limit=20
        )

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        # Unified view uses embed.description for "not available" message
        assert "embed" in call_kwargs
        assert "not available" in call_kwargs["embed"].description.lower()


class TestLeaderboardLimitParameter:
    """Tests for the limit parameter validation.

    Updated: The unified leaderboard view now silently clamps limits to 1-100
    instead of rejecting them with error messages.
    """

    @pytest.fixture
    def info_cog(self, player_repo):
        """Create an InfoCommands cog."""
        from commands.info import InfoCommands

        return InfoCommands(
            bot=MagicMock(),
            player_service=player_repo,
            match_service=MagicMock(),
            role_emojis={},
            role_names={},
        )

    @pytest.mark.asyncio
    async def test_leaderboard_invalid_limit_clamped(
        self, info_cog, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that invalid limits are clamped to valid range."""
        interaction = MockInteraction()
        interaction.guild.members = []  # Add members list for unified view

        await info_cog.leaderboard.callback(info_cog, interaction, type=None, limit=150)

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        # Should succeed with embed (limit clamped to 100)
        assert "embed" in call_kwargs
        assert "LEADERBOARD" in call_kwargs["embed"].title.upper()

    @pytest.mark.asyncio
    async def test_leaderboard_limit_zero_clamped(
        self, info_cog, mock_rate_limiter, mock_discord_helpers
    ):
        """Test that limit=0 is clamped to 1."""
        interaction = MockInteraction()
        interaction.guild.members = []  # Add members list for unified view

        await info_cog.leaderboard.callback(info_cog, interaction, type=None, limit=0)

        mock_followup = mock_discord_helpers["followup"]
        mock_followup.assert_called_once()
        call_kwargs = mock_followup.call_args.kwargs
        # Should succeed with embed (limit clamped to 1)
        assert "embed" in call_kwargs
        assert "LEADERBOARD" in call_kwargs["embed"].title.upper()


class TestLeaderboardGamblingContent:
    """Tests for gambling leaderboard content."""

    @pytest.fixture
    def info_cog_with_gambling(self, player_repo):
        """Create InfoCommands with gambling service that returns specific data."""
        from commands.info import InfoCommands
        from services.gambling_stats_service import Leaderboard, LeaderboardEntry, ServerStats

        # Use real dataclasses instead of MockGamblingLeaderboard
        server_stats: ServerStats = {
            "total_bets": 100,
            "total_wagered": 2000,
            "unique_gamblers": 10,
            "avg_bet_size": 20,
            "total_bankruptcies": 2,
        }
        mock_gambling_service = MagicMock()
        mock_gambling_service.get_leaderboard = MagicMock(
            return_value=Leaderboard(
                top_earners=[
                    LeaderboardEntry(discord_id=1001, total_bets=20, wins=15, losses=5, win_rate=0.75, net_pnl=500, total_wagered=400, avg_leverage=1.5),
                    LeaderboardEntry(discord_id=1002, total_bets=15, wins=9, losses=6, win_rate=0.60, net_pnl=200, total_wagered=300, avg_leverage=1.2),
                ],
                down_bad=[
                    LeaderboardEntry(discord_id=1003, total_bets=10, wins=3, losses=7, win_rate=0.30, net_pnl=-300, total_wagered=200, avg_leverage=2.0),
                ],
                hall_of_degen=[
                    LeaderboardEntry(discord_id=1004, total_bets=25, wins=10, losses=15, win_rate=0.40, net_pnl=-100, total_wagered=500, avg_leverage=3.0, degen_score=85, degen_emoji="🔥", degen_title="Mega Degen"),
                ],
                biggest_gamblers=[
                    LeaderboardEntry(discord_id=1001, total_bets=50, wins=30, losses=20, win_rate=0.60, net_pnl=500, total_wagered=1000, avg_leverage=2.0),
                ],
                total_wagered=2000,
                total_bets=100,
                avg_degen_score=50.0,
                total_bankruptcies=2,
                total_loans=5,
                server_stats=server_stats,
            )
        )

        return InfoCommands(
            bot=MagicMock(),
            player_service=player_repo,
            match_service=MagicMock(),
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


class TestGamblingLeaderboardIntegration:
    """Integration tests for gambling leaderboard using real service.

    These tests verify the actual Leaderboard dataclass has all required fields
    and that the command can access them with attribute syntax (not dict access).
    """

    @pytest.fixture
    def gambling_db_path(self, tmp_path):
        """Create a temporary database with schema for gambling tests."""
        from database import Database

        db_path = str(tmp_path / "test_gambling_integration.db")
        Database(db_path)
        return db_path

    @pytest.fixture
    def gambling_repos(self, gambling_db_path):
        """Create all repositories needed for gambling stats service."""
        from repositories.player_repository import PlayerRepository
        from repositories.bet_repository import BetRepository
        from repositories.match_repository import MatchRepository

        return {
            "player_repo": PlayerRepository(gambling_db_path),
            "bet_repo": BetRepository(gambling_db_path),
            "match_repo": MatchRepository(gambling_db_path),
        }

    @pytest.fixture
    def gambling_stats_service(self, gambling_repos):
        """Create a real GamblingStatsService for integration testing."""
        from services.gambling_stats_service import GamblingStatsService

        return GamblingStatsService(
            bet_repo=gambling_repos["bet_repo"],
            player_repo=gambling_repos["player_repo"],
            match_repo=gambling_repos["match_repo"],
            bankruptcy_service=None,
            loan_service=None,
        )

    def _seed_test_data(self, gambling_repos, guild_id=TEST_GUILD_ID):
        """Seed test data: players, matches, and bets directly in DB."""
        player_repo = gambling_repos["player_repo"]
        match_repo = gambling_repos["match_repo"]

        # Register 3 players
        for pid in [1001, 1002, 1003]:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=guild_id,
                initial_mmr=3000,
            )

        # Create 2 matches (team1=Radiant, team2=Dire)
        match1_id = match_repo.record_match(
            team1_ids=[1001, 1002],
            team2_ids=[1003],
            winning_team=1,  # Radiant won
            guild_id=guild_id,
        )
        match2_id = match_repo.record_match(
            team1_ids=[1001],
            team2_ids=[1002, 1003],
            winning_team=2,  # Dire won
            guild_id=guild_id,
        )

        # Insert bets directly into database (at least 3 per player)
        # Using direct SQL to bypass complex atomic betting logic
        with player_repo.connection() as conn:
            cursor = conn.cursor()

            # Player 1001: 4 bets, total wagered = 60 (10+20+10+20), 3 wins
            bets_1001 = [
                (guild_id, match1_id, 1001, "radiant", 10, 2, 0, 20),   # won, payout=20
                (guild_id, match1_id, 1001, "radiant", 10, 1, 0, 10),   # won, payout=10
                (guild_id, match2_id, 1001, "dire", 10, 2, 0, 20),      # won, payout=20
                (guild_id, match2_id, 1001, "radiant", 10, 1, 0, None), # lost, no payout
            ]
            # Player 1002: 3 bets, total wagered = 140 (100+20+20), mixed
            bets_1002 = [
                (guild_id, match1_id, 1002, "radiant", 20, 5, 0, 100),  # won, payout=100
                (guild_id, match1_id, 1002, "dire", 20, 1, 0, None),    # lost
                (guild_id, match2_id, 1002, "dire", 20, 1, 0, 20),      # won, payout=20
            ]
            # Player 1003: 3 bets, total wagered = 135 (45*3), all losses
            bets_1003 = [
                (guild_id, match1_id, 1003, "dire", 15, 3, 0, None),    # lost
                (guild_id, match1_id, 1003, "dire", 15, 3, 0, None),    # lost
                (guild_id, match2_id, 1003, "radiant", 15, 3, 0, None), # lost
            ]

            all_bets = bets_1001 + bets_1002 + bets_1003
            cursor.executemany(
                """
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, leverage, bet_time, payout)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                all_bets,
            )
            conn.commit()

    def test_leaderboard_has_biggest_gamblers_field(self, gambling_stats_service, gambling_repos):
        """Test that Leaderboard dataclass has biggest_gamblers field."""
        self._seed_test_data(gambling_repos)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, limit=5)

        # Verify biggest_gamblers exists and is a list
        assert hasattr(leaderboard, "biggest_gamblers"), "Leaderboard missing biggest_gamblers field"
        assert isinstance(leaderboard.biggest_gamblers, list)

    def test_leaderboard_has_server_stats_field(self, gambling_stats_service, gambling_repos):
        """Test that Leaderboard dataclass has server_stats field."""
        self._seed_test_data(gambling_repos)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, limit=5)

        # Verify server_stats exists and has required keys
        assert hasattr(leaderboard, "server_stats"), "Leaderboard missing server_stats field"
        assert "total_bets" in leaderboard.server_stats
        assert "total_wagered" in leaderboard.server_stats
        assert "unique_gamblers" in leaderboard.server_stats
        assert "avg_bet_size" in leaderboard.server_stats
        assert "total_bankruptcies" in leaderboard.server_stats

    def test_leaderboard_entry_has_degen_emoji(self, gambling_stats_service, gambling_repos):
        """Test that LeaderboardEntry has degen_emoji field."""
        self._seed_test_data(gambling_repos)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, limit=5)

        # hall_of_degen entries should have degen_emoji
        if leaderboard.hall_of_degen:
            entry = leaderboard.hall_of_degen[0]
            assert hasattr(entry, "degen_emoji"), "LeaderboardEntry missing degen_emoji field"

    def test_leaderboard_entry_has_total_wagered(self, gambling_stats_service, gambling_repos):
        """Test that LeaderboardEntry has total_wagered field."""
        self._seed_test_data(gambling_repos)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, limit=5)

        # All entries should have total_wagered
        if leaderboard.top_earners:
            entry = leaderboard.top_earners[0]
            assert hasattr(entry, "total_wagered"), "LeaderboardEntry missing total_wagered field"
            assert isinstance(entry.total_wagered, int)

    def test_biggest_gamblers_sorted_by_total_wagered(self, gambling_stats_service, gambling_repos):
        """Test that biggest_gamblers is sorted by total_wagered descending."""
        self._seed_test_data(gambling_repos)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, limit=5)

        if len(leaderboard.biggest_gamblers) >= 2:
            for i in range(len(leaderboard.biggest_gamblers) - 1):
                assert leaderboard.biggest_gamblers[i].total_wagered >= leaderboard.biggest_gamblers[i + 1].total_wagered

    def test_leaderboard_entries_use_attribute_access(self, gambling_stats_service, gambling_repos):
        """Test that LeaderboardEntry fields can be accessed with attribute syntax."""
        self._seed_test_data(gambling_repos)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, limit=5)

        # Verify attribute access works (not dict access)
        if leaderboard.top_earners:
            entry = leaderboard.top_earners[0]
            # These should NOT raise AttributeError
            _ = entry.discord_id
            _ = entry.net_pnl
            _ = entry.win_rate
            _ = entry.total_bets
            _ = entry.total_wagered

        if leaderboard.hall_of_degen:
            entry = leaderboard.hall_of_degen[0]
            _ = entry.degen_score
            _ = entry.degen_title
            _ = entry.degen_emoji


class TestLeaderboardPredictionsContent:
    """Tests for predictions leaderboard content."""

    @pytest.fixture
    def info_cog_with_predictions(self, player_repo):
        """Create InfoCommands with prediction service that returns specific data."""
        from commands.info import InfoCommands

        mock_prediction_service = MagicMock()
        mock_prediction_service.get_prediction_leaderboard = MagicMock(
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
        mock_prediction_service.get_server_prediction_stats = MagicMock(
            return_value={"total_predictions": 20, "total_bets": 50, "total_wagered": 500}
        )

        return InfoCommands(
            bot=MagicMock(),
            player_service=player_repo,
            match_service=MagicMock(),
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
