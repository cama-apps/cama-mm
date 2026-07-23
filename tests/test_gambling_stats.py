"""
Tests for gambling statistics and degen score functionality.
"""

import time
from unittest.mock import MagicMock

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.bet_repository import BetRepository
from repositories.loan_repository import LoanRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.bankruptcy_service import BankruptcyService
from services.gambling_stats_service import (
    DegenScoreBreakdown,
    GamblingStatsService,
    Leaderboard,
    _leverage_distribution_from_history,
)


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
        loan_repo=repositories["loan_repo"],
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
    """Helper to place and settle a bet through the production atomic path."""
    now = int(time.time())
    since_ts = now - 100

    # Place bet via the real atomic path (debits effective amount, stores leverage)
    bet_repo.place_bet_atomic(
        guild_id=guild_id,
        discord_id=discord_id,
        team=team,
        amount=amount,
        bet_time=now,
        since_ts=since_ts,
        leverage=leverage,
        max_debt=500,
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

    def test_bet_history_guild_isolation(self, repositories):
        """A bet placed in guild A does not appear in guild B's history."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        guild_a, guild_b = 111, 222
        discord_id = _setup_player(player_repo, balance=100, guild_id=guild_a)
        _setup_player(player_repo, balance=100, guild_id=guild_b)

        _place_and_settle_bet(
            bet_repo, match_repo, player_repo,
            discord_id, 10, "radiant", "radiant", guild_id=guild_a,
        )

        history_a = bet_repo.get_player_bet_history(discord_id, guild_id=guild_a)
        assert len(history_a) == 1
        assert bet_repo.get_player_bet_history(discord_id, guild_id=guild_b) == []


@pytest.mark.parametrize(
    ("history", "expected"),
    [
        ([], {}),
        (
            [
                {},
                {"leverage": None},
                {"leverage": 1},
                {"leverage": 2},
                {"leverage": 5},
                {"leverage": 5},
            ],
            {1: 3, 2: 1, 5: 2},
        ),
    ],
)
def test_leverage_distribution_from_history_matches_repository_semantics(
    history, expected
):
    """Missing and NULL leverage use the repository's 1x fallback."""
    assert _leverage_distribution_from_history(history) == expected


def test_bulk_gambling_metrics_match_legacy_aggregates(repositories):
    """One aggregate preserves the four legacy bet-history result sets."""
    bet_repo = repositories["bet_repo"]
    player_repo = repositories["player_repo"]
    match_repo = repositories["match_repo"]
    guild_id = 111
    other_guild_id = 222
    first_id = _setup_player(
        player_repo,
        discord_id=1001,
        balance=1000,
        guild_id=guild_id,
    )
    second_id = _setup_player(
        player_repo,
        discord_id=1002,
        balance=1000,
        guild_id=guild_id,
    )
    no_bets_id = _setup_player(
        player_repo,
        discord_id=1003,
        balance=1000,
        guild_id=guild_id,
    )
    _setup_player(
        player_repo,
        discord_id=first_id,
        balance=1000,
        guild_id=other_guild_id,
    )

    _place_and_settle_bet(
        bet_repo,
        match_repo,
        player_repo,
        first_id,
        10,
        "radiant",
        "dire",
        guild_id=guild_id,
    )
    _place_and_settle_bet(
        bet_repo,
        match_repo,
        player_repo,
        first_id,
        5,
        "radiant",
        "radiant",
        leverage=5,
        guild_id=guild_id,
    )
    _place_and_settle_bet(
        bet_repo,
        match_repo,
        player_repo,
        first_id,
        7,
        "radiant",
        "radiant",
        guild_id=guild_id,
    )
    _place_and_settle_bet(
        bet_repo,
        match_repo,
        player_repo,
        second_id,
        2,
        "radiant",
        "radiant",
        leverage=10,
        guild_id=guild_id,
    )
    _place_and_settle_bet(
        bet_repo,
        match_repo,
        player_repo,
        first_id,
        50,
        "radiant",
        "dire",
        guild_id=other_guild_id,
    )

    with bet_repo.connection() as conn:
        first_bets = conn.execute(
            """
            SELECT bet_id
            FROM bets
            WHERE guild_id = ? AND discord_id = ?
            ORDER BY bet_id
            """,
            (guild_id, first_id),
        ).fetchall()
        first_bet_ids = [row["bet_id"] for row in first_bets]
        conn.execute(
            """
            UPDATE bets
            SET bet_time = 123
            WHERE guild_id = ? AND discord_id = ?
            """,
            (guild_id, first_id),
        )
        conn.execute(
            "UPDATE bets SET leverage = NULL WHERE bet_id = ?",
            (first_bet_ids[0],),
        )
        conn.execute(
            "UPDATE bets SET payout = NULL WHERE bet_id = ?",
            (first_bet_ids[1],),
        )
        conn.execute(
            "UPDATE bets SET payout = 0 WHERE bet_id = ?",
            (first_bet_ids[2],),
        )

    metrics = bet_repo.get_bulk_gambling_metrics(guild_id)
    legacy_summaries = {
        row["discord_id"]: row
        for row in bet_repo.get_guild_gambling_summary(guild_id, min_bets=1)
    }
    player_ids = [first_id, second_id]
    legacy_leverage = bet_repo.get_bulk_leverage_distribution(
        guild_id,
        player_ids,
    )
    legacy_loss_chasing = bet_repo.get_bulk_loss_chasing_data(
        guild_id,
        player_ids,
    )
    legacy_unique_matches = bet_repo.get_bulk_unique_matches_bet_on(
        guild_id,
        player_ids,
    )

    assert set(metrics) == {first_id, second_id}
    for discord_id in player_ids:
        aggregate = metrics[discord_id]
        summary = legacy_summaries[discord_id]
        for key in (
            "total_bets",
            "wins",
            "losses",
            "total_wagered",
            "net_pnl",
            "win_rate",
            "roi",
            "avg_leverage",
        ):
            assert aggregate[key] == pytest.approx(summary[key])
        assert aggregate["five_x_bets"] == legacy_leverage[discord_id].get(5, 0)
        assert (
            aggregate["sequences_analyzed"]
            == legacy_loss_chasing[discord_id]["sequences_analyzed"]
        )
        assert (
            aggregate["times_increased_after_loss"]
            == legacy_loss_chasing[discord_id]["times_increased_after_loss"]
        )
        assert aggregate["unique_matches"] == legacy_unique_matches[discord_id]

    # The first player's three equal-time bets are ordered by bet_id:
    # loss 10 -> win 25 is one increased-after-loss sequence. A stored zero
    # payout on the final win remains zero rather than using the fallback.
    assert metrics[first_id]["sequences_analyzed"] == 1
    assert metrics[first_id]["times_increased_after_loss"] == 1
    assert metrics[first_id]["net_pnl"] == 8

    assert set(
        bet_repo.get_bulk_gambling_metrics(
            guild_id,
            [first_id, first_id, no_bets_id, 9999],
        )
    ) == {first_id}
    assert bet_repo.get_bulk_gambling_metrics(guild_id, []) == {}


def test_bulk_current_streaks_and_degen_scores_match_point_paths(
    gambling_stats_service, repositories
):
    bet_repo = repositories["bet_repo"]
    player_repo = repositories["player_repo"]
    match_repo = repositories["match_repo"]
    first_id = _setup_player(player_repo, discord_id=1001, balance=500)
    second_id = _setup_player(player_repo, discord_id=1002, balance=500)

    for winner in ("radiant", "dire", "radiant", "radiant"):
        _place_and_settle_bet(
            bet_repo,
            match_repo,
            player_repo,
            first_id,
            10,
            "radiant",
            winner,
        )
    for winner in ("radiant", "dire", "dire", "dire"):
        _place_and_settle_bet(
            bet_repo,
            match_repo,
            player_repo,
            second_id,
            10,
            "radiant",
            winner,
        )

    assert bet_repo.get_current_bet_streaks_bulk([first_id, second_id, 9999], 0) == {
        first_id: 2,
        second_id: -3,
    }

    bulk = gambling_stats_service.calculate_degen_scores_bulk([first_id, second_id], 0)
    assert bulk[first_id] == gambling_stats_service.calculate_degen_score(first_id, 0)
    assert bulk[second_id] == gambling_stats_service.calculate_degen_score(second_id, 0)


def test_bulk_degen_scores_use_consolidated_gambling_metrics(
    gambling_stats_service,
    repositories,
    monkeypatch,
):
    """Bulk scoring performs one scoped bet aggregate and no legacy scans."""
    bet_repo = repositories["bet_repo"]
    player_repo = repositories["player_repo"]
    match_repo = repositories["match_repo"]
    discord_id = _setup_player(player_repo, discord_id=1001, balance=500)
    _place_and_settle_bet(
        bet_repo,
        match_repo,
        player_repo,
        discord_id,
        10,
        "radiant",
        "radiant",
        leverage=5,
    )
    expected = gambling_stats_service.calculate_degen_score(discord_id, 0)

    calls = []
    aggregate = bet_repo.get_bulk_gambling_metrics

    def track_aggregate(guild_id, discord_ids=None):
        calls.append((guild_id, discord_ids))
        return aggregate(guild_id, discord_ids)

    monkeypatch.setattr(
        bet_repo,
        "get_bulk_gambling_metrics",
        track_aggregate,
    )
    for legacy_name in (
        "get_guild_gambling_summary",
        "get_bulk_leverage_distribution",
        "get_bulk_loss_chasing_data",
        "get_bulk_unique_matches_bet_on",
    ):
        monkeypatch.setattr(
            bet_repo,
            legacy_name,
            MagicMock(side_effect=AssertionError(f"{legacy_name} was called")),
        )

    scores = gambling_stats_service.calculate_degen_scores_bulk(
        [discord_id, discord_id, 9999],
        0,
    )

    assert calls == [(0, [discord_id, 9999])]
    assert scores[discord_id] == expected
    assert scores[9999].total == 0


def test_gambling_leaderboard_uses_consolidated_metrics(
    gambling_stats_service,
    repositories,
    monkeypatch,
):
    """Leaderboard derives every bet metric from one guild aggregate."""
    bet_repo = repositories["bet_repo"]
    player_repo = repositories["player_repo"]
    match_repo = repositories["match_repo"]
    discord_id = _setup_player(player_repo, discord_id=1001, balance=500)
    for _ in range(3):
        _place_and_settle_bet(
            bet_repo,
            match_repo,
            player_repo,
            discord_id,
            10,
            "radiant",
            "radiant",
        )

    calls = []
    aggregate = bet_repo.get_bulk_gambling_metrics

    def track_aggregate(guild_id, discord_ids=None):
        calls.append((guild_id, discord_ids))
        return aggregate(guild_id, discord_ids)

    monkeypatch.setattr(
        bet_repo,
        "get_bulk_gambling_metrics",
        track_aggregate,
    )
    for legacy_name in (
        "get_guild_gambling_summary",
        "get_bulk_leverage_distribution",
        "get_bulk_loss_chasing_data",
        "get_bulk_unique_matches_bet_on",
    ):
        monkeypatch.setattr(
            bet_repo,
            legacy_name,
            MagicMock(side_effect=AssertionError(f"{legacy_name} was called")),
        )

    leaderboard = gambling_stats_service.get_leaderboard(0, min_bets=3)

    assert calls == [(0, None)]
    assert [entry.discord_id for entry in leaderboard.top_earners] == [discord_id]
    assert leaderboard.server_stats["total_bets"] == 3


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

    def test_get_player_stats_derives_leverage_from_history(
        self, gambling_stats_service, repositories, monkeypatch
    ):
        """Loaded history replaces the leverage query without changing output."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]
        discord_id = _setup_player(player_repo, balance=500)
        for leverage in (1, 2, 5):
            _place_and_settle_bet(
                bet_repo,
                match_repo,
                player_repo,
                discord_id,
                10,
                "radiant",
                "radiant",
                leverage=leverage,
            )

        # Exercise the repository's historical NULL -> 1x compatibility path.
        with bet_repo.connection() as conn:
            conn.execute(
                "UPDATE bets SET leverage = NULL WHERE discord_id = ? AND leverage = 1",
                (discord_id,),
            )

        history = bet_repo.get_player_bet_history(discord_id, 0)
        expected_distribution = bet_repo.get_player_leverage_distribution(discord_id, 0)
        expected_degen = gambling_stats_service.calculate_degen_score(
            discord_id,
            0,
            history=history,
            leverage_distribution=expected_distribution,
        )

        history_spy = MagicMock(wraps=bet_repo.get_player_bet_history)
        leverage_spy = MagicMock(wraps=bet_repo.get_player_leverage_distribution)
        monkeypatch.setattr(bet_repo, "get_player_bet_history", history_spy)
        monkeypatch.setattr(
            bet_repo,
            "get_player_leverage_distribution",
            leverage_spy,
        )

        stats = gambling_stats_service.get_player_stats(discord_id, guild_id=0)

        assert stats is not None
        assert stats.leverage_distribution == expected_distribution
        assert stats.degen_score == expected_degen
        history_spy.assert_called_once_with(discord_id, 0)
        leverage_spy.assert_not_called()

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

    def test_calculate_degen_score_derives_leverage_without_repository_query(
        self, gambling_stats_service, repositories, monkeypatch
    ):
        """The public default path reads history once and derives leverage from it."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]
        discord_id = _setup_player(player_repo, balance=200)
        _place_and_settle_bet(
            bet_repo,
            match_repo,
            player_repo,
            discord_id,
            10,
            "radiant",
            "radiant",
            leverage=5,
        )

        history_spy = MagicMock(wraps=bet_repo.get_player_bet_history)
        leverage_spy = MagicMock(wraps=bet_repo.get_player_leverage_distribution)
        monkeypatch.setattr(bet_repo, "get_player_bet_history", history_spy)
        monkeypatch.setattr(
            bet_repo,
            "get_player_leverage_distribution",
            leverage_spy,
        )

        degen = gambling_stats_service.calculate_degen_score(discord_id, guild_id=0)

        assert degen.max_leverage_score == 25
        history_spy.assert_called_once_with(discord_id, 0)
        leverage_spy.assert_not_called()

    def test_calculate_degen_score_reuses_prefetched_empty_bet_data(
        self, gambling_stats_service, repositories, monkeypatch
    ):
        """Explicit empty prefetches are data, not a signal to query again."""
        bet_repo = repositories["bet_repo"]
        history_spy = MagicMock(wraps=bet_repo.get_player_bet_history)
        leverage_spy = MagicMock(wraps=bet_repo.get_player_leverage_distribution)
        monkeypatch.setattr(bet_repo, "get_player_bet_history", history_spy)
        monkeypatch.setattr(
            bet_repo,
            "get_player_leverage_distribution",
            leverage_spy,
        )

        degen = gambling_stats_service.calculate_degen_score(
            1001,
            guild_id=0,
            history=[],
            leverage_distribution={},
        )

        assert degen.total == 0
        history_spy.assert_not_called()
        leverage_spy.assert_not_called()

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

    def test_server_stats_include_bettors_below_minimum(
        self, gambling_stats_service, repositories
    ):
        """The ranking threshold must not filter server-wide footer totals."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]
        bankruptcy_repo = repositories["bankruptcy_repo"]

        eligible_id = _setup_player(player_repo, discord_id=1001, balance=200)
        for _ in range(3):
            _place_and_settle_bet(
                bet_repo,
                match_repo,
                player_repo,
                eligible_id,
                10,
                "radiant",
                "radiant",
            )

        ineligible_id = _setup_player(player_repo, discord_id=1002, balance=200)
        for _ in range(2):
            _place_and_settle_bet(
                bet_repo,
                match_repo,
                player_repo,
                ineligible_id,
                20,
                "radiant",
                "dire",
            )
        bankruptcy_repo.upsert_state(ineligible_id, 0, 1, 0)

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, min_bets=3)

        assert [entry.discord_id for entry in leaderboard.top_earners] == [eligible_id]
        assert all(
            entry.discord_id != ineligible_id
            for section in (
                leaderboard.top_earners,
                leaderboard.down_bad,
                leaderboard.hall_of_degen,
                leaderboard.biggest_gamblers,
            )
            for entry in section
        )
        assert leaderboard.total_bets == 5
        assert leaderboard.total_wagered == 70
        assert leaderboard.total_bankruptcies == 1
        assert leaderboard.server_stats == {
            "total_bets": 5,
            "total_wagered": 70,
            "unique_gamblers": 2,
            "avg_bet_size": 14,
            "total_bankruptcies": 1,
        }

    def test_leaderboard_with_only_ineligible_bettors_keeps_server_stats(
        self, gambling_stats_service, repositories
    ):
        """Footer totals remain populated when no bettor qualifies to rank."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, discord_id=1001, balance=100)
        _place_and_settle_bet(
            bet_repo,
            match_repo,
            player_repo,
            discord_id,
            25,
            "radiant",
            "radiant",
        )

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, min_bets=3)

        assert leaderboard.top_earners == []
        assert leaderboard.down_bad == []
        assert leaderboard.hall_of_degen == []
        assert leaderboard.biggest_gamblers == []
        assert leaderboard.server_stats["total_bets"] == 1
        assert leaderboard.server_stats["total_wagered"] == 25
        assert leaderboard.server_stats["unique_gamblers"] == 1

    def test_leaderboard_section_ties_use_discord_id(
        self, gambling_stats_service, repositories
    ):
        """All section rankings have a stable discord-id tie breaker."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        for discord_id, winning_team in (
            (1002, "radiant"),
            (2002, "dire"),
            (1001, "radiant"),
            (2001, "dire"),
        ):
            _setup_player(player_repo, discord_id=discord_id, balance=200)
            for _ in range(3):
                _place_and_settle_bet(
                    bet_repo,
                    match_repo,
                    player_repo,
                    discord_id,
                    10,
                    "radiant",
                    winning_team,
                )

        leaderboard = gambling_stats_service.get_leaderboard(
            guild_id=0, min_bets=3, limit=10
        )

        assert [entry.discord_id for entry in leaderboard.top_earners] == [1001, 1002]
        assert [entry.discord_id for entry in leaderboard.down_bad] == [2001, 2002]
        assert [entry.discord_id for entry in leaderboard.hall_of_degen] == [
            1001,
            1002,
            2001,
            2002,
        ]
        assert [entry.discord_id for entry in leaderboard.biggest_gamblers] == [
            1001,
            1002,
            2001,
            2002,
        ]

    def test_top_earners_excludes_negative_pnl(
        self, gambling_stats_service, repositories
    ):
        """Losing bettors belong in Down Bad, not Top Earners."""
        bet_repo = repositories["bet_repo"]
        player_repo = repositories["player_repo"]
        match_repo = repositories["match_repo"]

        discord_id = _setup_player(player_repo, discord_id=1001, balance=200)
        for _ in range(3):
            _place_and_settle_bet(
                bet_repo,
                match_repo,
                player_repo,
                discord_id,
                10,
                "radiant",
                "dire",
            )

        leaderboard = gambling_stats_service.get_leaderboard(guild_id=0, min_bets=3)

        assert leaderboard.top_earners == []
        assert [entry.discord_id for entry in leaderboard.down_bad] == [discord_id]

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
        match_repo.record_match(
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
