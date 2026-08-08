"""
Tests for the /leaderboard command type parameter routing.

Tests that the type parameter correctly routes to:
- balance (default)
- gambling
"""

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def temp_db_path(repo_db_path):
    """Temporary database with schema (session template copy — fast)."""
    return repo_db_path


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


def create_mock_members(player_ids):
    """Create mock guild members for the given player IDs."""
    members = []
    for pid in player_ids:
        member = MagicMock()
        member.id = pid
        member.display_name = f"Player{pid}"
        members.append(member)
    return members


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
        # Include mock members for common test IDs used in fixtures
        self.guild.members = create_mock_members([1001, 1002, 1003, 1004, 1005])
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

        return InfoCommands(
            bot=mock_bot,
            player_service=player_repo,
            match_service=mock_match_repo,
            gambling_stats_service=mock_gambling_service,
            bankruptcy_service=None,
        )

    @pytest.mark.asyncio
    async def test_leaderboard_default_is_balance(
        self, info_cog, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Leaderboard with no type defaults to balance, and type=balance
        routes to the same balance leaderboard."""
        player_ids = [1, 2, 3, 4, 5]
        register_players(player_repo, player_ids)
        mock_followup = mock_discord_helpers["followup"]

        for type_choice in (None, MockChoice("balance")):
            mock_followup.reset_mock()
            interaction = MockInteraction()
            interaction.guild.members = create_mock_members(player_ids)

            await info_cog.leaderboard.callback(
                info_cog, interaction, type=type_choice, limit=20
            )

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
    async def test_gambling_leaderboard_unavailable_service(
        self, player_repo, mock_rate_limiter, mock_discord_helpers
    ):
        """Test graceful handling when gambling service unavailable."""
        from commands.info import InfoCommands

        info_cog = InfoCommands(
            bot=MagicMock(),
            player_service=player_repo,
            match_service=MagicMock(),
            gambling_stats_service=None,
            prediction_service=None,
        )

        interaction = MockInteraction()
        interaction.guild.members = []  # Will be populated with matching players

        await info_cog.leaderboard.callback(
            info_cog, interaction, type=MockChoice("gambling"), limit=20
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
        )

    @pytest.mark.asyncio
    async def test_leaderboard_invalid_limit_clamped(
        self, info_cog, mock_rate_limiter, mock_discord_helpers
    ):
        """Out-of-range limits (150 -> 100, 0 -> 1) are clamped, not rejected."""
        mock_followup = mock_discord_helpers["followup"]

        for limit in (150, 0):
            mock_followup.reset_mock()
            interaction = MockInteraction()
            interaction.guild.members = []  # Will be populated with matching players

            await info_cog.leaderboard.callback(info_cog, interaction, type=None, limit=limit)

            mock_followup.assert_called_once()
            call_kwargs = mock_followup.call_args.kwargs
            # Should succeed with embed (limit clamped into 1-100)
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
    def gambling_db_path(self, repo_db_path):
        """Temporary database with schema (session template copy — fast)."""
        return repo_db_path

    @pytest.fixture
    def gambling_repos(self, gambling_db_path):
        """Create all repositories needed for gambling stats service."""
        from repositories.bet_repository import BetRepository
        from repositories.match_repository import MatchRepository
        from repositories.player_repository import PlayerRepository

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

    def test_leaderboard_dataclass_shape_and_sorting(self, gambling_stats_service, gambling_repos):
        """One real get_leaderboard call verifies the dataclass surface the
        command relies on: biggest_gamblers populated (even when nobody has
        positive P&L for Top Earners) and sorted by total_wagered descending,
        server_stats carrying all footer keys, and every entry field the embed
        reads being attribute-accessible (incl. degen fields on hall_of_degen)."""
        self._seed_test_data(gambling_repos)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=TEST_GUILD_ID, limit=5)

        # Every qualifying bettor appears in biggest_gamblers even when nobody
        # has positive P&L for the Top Earners section — sorted by total_wagered.
        assert isinstance(leaderboard.biggest_gamblers, list)
        assert len(leaderboard.biggest_gamblers) >= 2, "seed data should populate biggest_gamblers"
        for i in range(len(leaderboard.biggest_gamblers) - 1):
            assert leaderboard.biggest_gamblers[i].total_wagered >= leaderboard.biggest_gamblers[i + 1].total_wagered

        # server_stats has every footer key.
        for key in (
            "total_bets",
            "total_wagered",
            "unique_gamblers",
            "avg_bet_size",
            "total_bankruptcies",
        ):
            assert key in leaderboard.server_stats, f"server_stats missing {key}"

        # Entry fields the embed reads are attribute-accessible (not dict access).
        entry = leaderboard.biggest_gamblers[0]
        assert isinstance(entry.total_wagered, int)
        _ = entry.discord_id
        _ = entry.net_pnl
        _ = entry.win_rate
        _ = entry.total_bets

        assert leaderboard.hall_of_degen, "seed data should populate hall_of_degen"
        entry = leaderboard.hall_of_degen[0]
        _ = entry.degen_score
        _ = entry.degen_title
        _ = entry.degen_emoji
