"""
Tests for gambling statistics and degen score functionality.
"""

import time

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from repositories.bankruptcy_repository import BankruptcyRepository
from services.bankruptcy_service import BankruptcyService
from services.gambling_stats_service import (
    DegenScoreBreakdown,
    GamblingStatsService,
    Leaderboard,
)
from repositories.loan_repository import LoanRepository


@pytest.fixture
def db_path(tmp_path):
    """Create a temporary database with schema."""
    db = str(tmp_path / "test_gamba.db")
    schema = SchemaManager(db)
    schema.initialize()
    return db


@pytest.fixture
def repositories(db_path):
    """Create repositories for testing."""
    return {
        "player_repo": PlayerRepository(db_path),
        "bet_repo": BetRepository(db_path),
        "match_repo": MatchRepository(db_path),
        "bankruptcy_repo": BankruptcyRepository(db_path),
        "loan_repo": LoanRepository(db_path),
    }


@pytest.fixture
def gambling_stats_service(repositories):
    """Create gambling stats service for testing."""
    bankruptcy_service = BankruptcyService(
        repositories["bankruptcy_repo"],
        repositories["player_repo"],
    )
    return GamblingStatsService(
        bet_repo=repositories["bet_repo"],
        player_repo=repositories["player_repo"],
        match_repo=repositories["match_repo"],
        bankruptcy_service=bankruptcy_service,
    )


def _setup_player(player_repo, discord_id=1001, balance=100, guild_id=0):
    """Helper to create a test player."""
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"TestPlayer{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
    )
    player_repo.update_balance(discord_id, guild_id, balance)
    return discord_id


def _place_and_settle_bet(
    bet_repo,
    match_repo,
    player_repo,
    discord_id,
    amount,
    team,
    winning_team,
    leverage=1,
    guild_id=0,
):
    """Helper to place and settle a bet."""
    now = int(time.time())
    since_ts = now - 100

    # Debit balance
    effective = amount * leverage
    player_repo.add_balance(discord_id, guild_id, -effective)

    # Place bet
    bet_repo.create_bet(guild_id, discord_id, team, amount, now)
    # Update leverage manually since create_bet doesn't support it
    with bet_repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE bets SET leverage = ? WHERE discord_id = ? AND match_id IS NULL",
            (leverage, discord_id),
        )

    # Record match
    match_id = match_repo.record_match(
        team1_ids=[discord_id] if team == "radiant" else [999],
        team2_ids=[999] if team == "radiant" else [discord_id],
        winning_team=1 if winning_team == "radiant" else 2,
        guild_id=guild_id,
    )

    # Settle bet
    bet_repo.settle_pending_bets_atomic(
        match_id=match_id,
        guild_id=guild_id,
        since_ts=since_ts,
        winning_team=winning_team,
        house_payout_multiplier=1.0,
        betting_mode="house",
    )

    return match_id


class TestBetHistory:
    """Tests for bet history retrieval."""

    def test_get_player_bet_history_empty(self, repositories):
        """Test getting history for player with no bets."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        _setup_player(player_repo)

        history = bet_repo.get_player_bet_history(1001)
        assert history == []

    def test_get_player_bet_history_with_bets(self, repositories):
        """Test getting history for player with settled bets."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=100)

        # Win a bet
        _place_and_settle_bet(
            bet_repo, match_repo, player_repo,
            discord_id, 10, "radiant", "radiant"
        )

        history = bet_repo.get_player_bet_history(discord_id)
        assert len(history) == 1
        assert history[0]["outcome"] == "won"
        assert history[0]["profit"] == 10  # Won back effective bet
        assert history[0]["amount"] == 10
        assert history[0]["leverage"] == 1

    def test_bet_history_tracks_losses(self, repositories):
        """Test that losses are tracked correctly."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=100)

        # Lose a bet
        _place_and_settle_bet(
            bet_repo, match_repo, player_repo,
            discord_id, 15, "radiant", "dire"
        )

        history = bet_repo.get_player_bet_history(discord_id)
        assert len(history) == 1
        assert history[0]["outcome"] == "lost"
        assert history[0]["profit"] == -15  # Lost effective bet

    def test_bet_history_with_leverage(self, repositories):
        """Test that leverage is reflected in history."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=100)

        # Win a leveraged bet
        _place_and_settle_bet(
            bet_repo, match_repo, player_repo,
            discord_id, 10, "radiant", "radiant",
            leverage=2,
        )

        history = bet_repo.get_player_bet_history(discord_id)
        assert len(history) == 1
        assert history[0]["leverage"] == 2
        assert history[0]["effective_bet"] == 20
        assert history[0]["profit"] == 20  # effective_bet profit on win


class TestGambaStats:
    """Tests for gambling statistics."""

    def test_get_player_stats_no_bets(self, gambling_stats_service, repositories):
        """Test stats for player with no bets returns None."""
        player_repo = repositories["player_repo"]
        _setup_player(player_repo)

        stats = gambling_stats_service.get_player_stats(1001, guild_id=0)
        assert stats is None

    def test_get_player_stats_with_bets(self, gambling_stats_service, repositories):
        """Test stats calculation for player with bets."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=200)

        # Win 2, lose 1
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "dire")

        stats = gambling_stats_service.get_player_stats(discord_id, guild_id=0)

        assert stats is not None
        assert stats.total_bets == 3
        assert stats.wins == 2
        assert stats.losses == 1
        assert stats.win_rate == pytest.approx(2/3)
        assert stats.net_pnl == 10  # +10 +10 -10
        assert stats.total_wagered == 30

    def test_streak_calculation(self, gambling_stats_service, repositories):
        """Test that streaks are calculated correctly."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=200)

        # W W W L L (ends on L2 streak)
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 5, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 5, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 5, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 5, "radiant", "dire")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 5, "radiant", "dire")

        stats = gambling_stats_service.get_player_stats(discord_id, guild_id=0)

        assert stats.best_streak == 3
        assert stats.worst_streak == -2
        assert stats.current_streak == -2


class TestDegenScore:
    """Tests for degen score calculation."""

    def test_degen_score_basic(self, gambling_stats_service, repositories):
        """Test basic degen score calculation."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=200)

        # Place a few simple 1x bets
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "dire")

        degen = gambling_stats_service.calculate_degen_score(discord_id, guild_id=0)

        assert isinstance(degen, DegenScoreBreakdown)
        assert 0 <= degen.total <= 100
        assert degen.title in ["Casual", "Recreational", "Committed", "Degenerate", "Menace", "Legendary Degen"]

    def test_high_leverage_increases_degen_score(self, gambling_stats_service, repositories):
        """Test that high leverage increases degen score."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        # Player 1: only 1x bets
        discord_id1 = _setup_player(player_repo, discord_id=1001, balance=200)
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id1, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id1, 10, "radiant", "radiant")

        # Player 2: 5x leverage bets
        discord_id2 = _setup_player(player_repo, discord_id=1002, balance=200)
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id2, 10, "radiant", "radiant", leverage=5)
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id2, 10, "radiant", "radiant", leverage=5)

        degen1 = gambling_stats_service.calculate_degen_score(discord_id1, guild_id=0)
        degen2 = gambling_stats_service.calculate_degen_score(discord_id2, guild_id=0)

        assert degen2.max_leverage_score > degen1.max_leverage_score
        assert degen2.total > degen1.total


class TestLeaderboard:
    """Tests for gambling leaderboard."""

    def test_leaderboard_empty(self, gambling_stats_service, repositories):
        """Test leaderboard with no bets."""
        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0)

        assert isinstance(leaderboard, Leaderboard)
        assert len(leaderboard.top_earners) == 0
        assert len(leaderboard.down_bad) == 0
        assert len(leaderboard.hall_of_degen) == 0

    def test_leaderboard_min_bets_filter(self, gambling_stats_service, repositories):
        """Test that players with fewer than min_bets are excluded."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        # Player 1: 2 bets (below minimum of 3)
        discord_id1 = _setup_player(player_repo, discord_id=1001, balance=100)
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id1, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id1, 10, "radiant", "radiant")

        # Player 2: 3 bets (meets minimum)
        discord_id2 = _setup_player(player_repo, discord_id=1002, balance=100)
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id2, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id2, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id2, 10, "radiant", "radiant")

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, min_bets=3)

        # Only player 2 should appear
        assert len(leaderboard.top_earners) == 1
        assert leaderboard.top_earners[0].discord_id == discord_id2

    def test_leaderboard_sections(self, gambling_stats_service, repositories):
        """Test that leaderboard correctly categorizes players."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        # Winner
        winner_id = _setup_player(player_repo, discord_id=1001, balance=200)
        for _ in range(5):
            _place_and_settle_bet(bet_repo, match_repo, player_repo, winner_id, 10, "radiant", "radiant")

        # Loser
        loser_id = _setup_player(player_repo, discord_id=1002, balance=200)
        for _ in range(5):
            _place_and_settle_bet(bet_repo, match_repo, player_repo, loser_id, 10, "radiant", "dire")

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, min_bets=3)

        # Winner should be in top earners
        assert any(e.discord_id == winner_id for e in leaderboard.top_earners)
        assert leaderboard.top_earners[0].net_pnl > 0

        # Loser should be in down bad
        assert any(e.discord_id == loser_id for e in leaderboard.down_bad)
        assert leaderboard.down_bad[0].net_pnl < 0

    def test_leaderboard_total_loans(self, gambling_stats_service, repositories):
        """Test that total_loans is a server-wide aggregate stat."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]
        loan_repo = repositories["loan_repo"]

        # Player 1 with 5 loans and 3 bets
        player1 = _setup_player(player_repo, discord_id=1001, balance=100)
        loan_repo.upsert_state(discord_id=player1, total_loans_taken=5)
        for _ in range(3):
            _place_and_settle_bet(bet_repo, match_repo, player_repo, player1, 10, "radiant", "radiant")

        # Player 2 with 3 loans and 3 bets
        player2 = _setup_player(player_repo, discord_id=1002, balance=100)
        loan_repo.upsert_state(discord_id=player2, total_loans_taken=3)
        for _ in range(3):
            _place_and_settle_bet(bet_repo, match_repo, player_repo, player2, 10, "radiant", "dire")

        # Player 3 with 10 loans but only 2 bets (below min_bets but still counts for server stats)
        player3 = _setup_player(player_repo, discord_id=1003, balance=100)
        loan_repo.upsert_state(discord_id=player3, total_loans_taken=10)
        for _ in range(2):
            _place_and_settle_bet(bet_repo, match_repo, player_repo, player3, 10, "radiant", "radiant")

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, min_bets=3)

        # Server-wide stat: counts ALL loans (5 + 3 + 10 = 18)
        assert leaderboard.total_loans == 18

    def test_leaderboard_total_loans_empty(self, gambling_stats_service, repositories):
        """Test that total_loans is 0 when no players have bets."""
        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0)
        assert leaderboard.total_loans == 0


class TestPnlSeries:
    """Tests for cumulative P&L series generation."""

    def test_cumulative_pnl_series(self, gambling_stats_service, repositories):
        """Test cumulative P&L series generation."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=200)

        # W (+10), L (-10), W (+10) = cumulative: 10, 0, 10
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "radiant")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "dire")
        _place_and_settle_bet(bet_repo, match_repo, player_repo, discord_id, 10, "radiant", "radiant")

        series = gambling_stats_service.get_cumulative_pnl_series(discord_id, guild_id=0)

        assert len(series) == 3
        assert series[0] == (1, 10, pytest.approx({"amount": 10, "leverage": 1, "effective_bet": 10, "outcome": "won", "profit": 10, "team": "radiant", "source": "bet"}, rel=1e-2))
        assert series[1][1] == 0  # 10 - 10 = 0
        assert series[2][1] == 10  # 0 + 10 = 10

    def test_cumulative_pnl_series_with_double_or_nothing(self, gambling_stats_service, repositories):
        """Test cumulative P&L series includes Double or Nothing spins."""
        player_repo = repositories["player_repo"]

        discord_id = _setup_player(player_repo, balance=1000)
        now = int(time.time())

        # Simulate a Double or Nothing WIN:
        # Player has 1000 balance, pays 50 cost, has 950 at risk, doubles to 1900
        # Profit = 1900 - (950 + 50) = 1900 - 1000 = +900
        player_repo.log_double_or_nothing(
            discord_id=discord_id,
            guild_id=0,
            cost=50,
            balance_before=950,  # Balance after cost deducted
            balance_after=1900,  # Doubled from balance_before
            won=True,
            spin_time=now,
        )

        # Simulate a Double or Nothing LOSS:
        # Player has 1900 balance, pays 50 cost, has 1850 at risk, loses everything
        # Profit = 0 - (1850 + 50) = 0 - 1900 = -1900
        player_repo.log_double_or_nothing(
            discord_id=discord_id,
            guild_id=0,
            cost=50,
            balance_before=1850,
            balance_after=0,
            won=False,
            spin_time=now + 100,
        )

        series = gambling_stats_service.get_cumulative_pnl_series(discord_id, guild_id=0)

        assert len(series) == 2

        # First event: DoN win
        assert series[0][0] == 1  # Event number
        assert series[0][1] == 900  # Cumulative P&L
        assert series[0][2]["source"] == "double_or_nothing"
        assert series[0][2]["outcome"] == "won"
        assert series[0][2]["profit"] == 900
        assert series[0][2]["amount"] == 1000  # Original balance (before cost)
        assert series[0][2]["effective_bet"] == 950  # Amount at risk

        # Second event: DoN loss
        assert series[1][0] == 2  # Event number
        assert series[1][1] == 900 - 1900  # Cumulative: 900 - 1900 = -1000
        assert series[1][2]["source"] == "double_or_nothing"
        assert series[1][2]["outcome"] == "lost"
        assert series[1][2]["profit"] == -1900
        assert series[1][2]["amount"] == 1900  # Original balance
        assert series[1][2]["effective_bet"] == 1850  # Amount at risk


class TestPaperHands:
    """Tests for paper hands detection."""

    def test_paper_hands_detection(self, repositories):
        """Test detection of matches played without betting on self."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=100)

        # Record a match where player was on radiant (team 1)
        match_id = match_repo.record_match(
            team1_ids=[discord_id],
            team2_ids=[999],
            winning_team=1,
            guild_id=0,
        )

        # No bet placed on this match
        result = bet_repo.get_player_matches_without_self_bet(discord_id)

        assert result["matches_played"] == 1
        assert result["paper_hands_count"] == 1
        assert result["matches_bet_on_self"] == 0


class TestPayoutStorage:
    """Tests for payout column storage."""

    def test_payout_stored_on_settlement(self, repositories):
        """Test that payout is stored when bet is settled."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=100)

        _place_and_settle_bet(
            bet_repo, match_repo, player_repo,
            discord_id, 10, "radiant", "radiant"
        )

        history = bet_repo.get_player_bet_history(discord_id)
        assert len(history) == 1
        assert history[0]["payout"] == 20  # 10 * 2 (house mode 1:1)

    def test_payout_null_for_losers(self, repositories):
        """Test that payout is NULL for losing bets."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, balance=100)

        _place_and_settle_bet(
            bet_repo, match_repo, player_repo,
            discord_id, 10, "radiant", "dire"
        )

        # Check raw payout in DB
        with bet_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT payout FROM bets WHERE discord_id = ?", (discord_id,))
            row = cursor.fetchone()
            assert row["payout"] is None
