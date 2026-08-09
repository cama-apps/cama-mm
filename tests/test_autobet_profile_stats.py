from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.gambling_stats_service import (
    GamblingStatsService,
    _calculate_auto_bet_performance,
)
from tests.conftest import TEST_GUILD_ID


def _history_bet(
    *,
    match_id: int,
    team: str,
    automatic: bool,
    wagered: int,
    profit: int,
    won: bool,
    target_id: int | None = None,
    direction: str | None = None,
) -> dict:
    return {
        "match_id": match_id,
        "team_bet_on": team,
        "is_blind": int(automatic),
        "investment_target_id": target_id,
        "investment_direction": direction,
        "effective_bet": wagered,
        "profit": profit,
        "outcome": "won" if won else "lost",
    }


def _add_player(player_repo: PlayerRepository, discord_id: int, balance: int = 100) -> None:
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def test_real_investment_bet_persists_attribution_and_uses_settled_pnl(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    betting_service = BettingService(bet_repo, player_repo)

    investor_id = 101
    target_id = 201
    opponent_id = 202
    for discord_id in (investor_id, target_id, opponent_id):
        _add_player(player_repo, discord_id)

    bet_repo.set_investment(
        TEST_GUILD_ID,
        investor_id=investor_id,
        target_id=target_id,
        direction="long",
        percentage=10,
    )
    shuffle_timestamp = 1_700_000_000
    prepared = betting_service.prepare_investments(
        TEST_GUILD_ID,
        target_ids=[target_id, opponent_id],
    )

    result = betting_service.create_investment_bets(
        guild_id=TEST_GUILD_ID,
        radiant_ids=[target_id],
        dire_ids=[opponent_id],
        shuffle_timestamp=shuffle_timestamp,
        prepared_investments=prepared,
    )

    assert result["created"] == 1
    match_id = match_repo.record_match(
        team1_ids=[target_id],
        team2_ids=[opponent_id],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
        betting_mode="house",
    )
    bet_repo.settle_pending_bets_atomic(
        match_id=match_id,
        guild_id=TEST_GUILD_ID,
        since_ts=shuffle_timestamp,
        winning_team="radiant",
        house_payout_multiplier=1.0,
        betting_mode="house",
    )

    history = bet_repo.get_player_bet_history(investor_id, TEST_GUILD_ID)
    assert len(history) == 1
    assert history[0]["is_blind"] == 1
    assert history[0]["investment_target_id"] == target_id
    assert history[0]["investment_direction"] == "long"
    assert history[0]["effective_bet"] == 10
    assert history[0]["profit"] == 10

    performance = _calculate_auto_bet_performance(history)
    assert performance.total.net_pnl == 10
    assert performance.generic.bet_count == 0
    assert performance.targets[0].target_id == target_id
    assert performance.targets[0].directions == ("long",)
    assert performance.targets[0].net_pnl == 10

    profile_stats = GamblingStatsService(
        bet_repo=bet_repo,
        player_repo=player_repo,
        match_repo=match_repo,
    ).get_player_stats(investor_id, TEST_GUILD_ID)
    assert profile_stats is not None
    assert profile_stats.auto_bet_performance.targets[0].target_id == target_id
    assert profile_stats.auto_bet_performance.targets[0].net_pnl == 10


def test_auto_bet_performance_groups_generic_and_each_target():
    history = [
        _history_bet(
            match_id=1,
            team="radiant",
            automatic=True,
            wagered=10,
            profit=8,
            won=True,
        ),
        _history_bet(
            match_id=2,
            team="dire",
            automatic=True,
            wagered=20,
            profit=15,
            won=True,
            target_id=9001,
            direction="long",
        ),
        _history_bet(
            match_id=3,
            team="radiant",
            automatic=True,
            wagered=5,
            profit=-5,
            won=False,
            target_id=9001,
            direction="short",
        ),
        _history_bet(
            match_id=4,
            team="dire",
            automatic=True,
            wagered=12,
            profit=-12,
            won=False,
            target_id=9002,
            direction="short",
        ),
    ]

    performance = _calculate_auto_bet_performance(history)

    assert performance.total.bet_count == 4
    assert performance.total.total_wagered == 47
    assert performance.total.net_pnl == 6
    assert performance.generic.bet_count == 1
    assert performance.generic.net_pnl == 8
    by_target = {group.target_id: group for group in performance.targets}
    assert by_target[9001].directions == ("long", "short")
    assert by_target[9001].bet_count == 2
    assert by_target[9001].wins == 1
    assert by_target[9001].total_wagered == 25
    assert by_target[9001].net_pnl == 10
    assert by_target[9002].directions == ("short",)
    assert by_target[9002].net_pnl == -12


def test_opposite_manual_arbitrage_is_counted_once_with_multiple_opposing_autos():
    history = [
        _history_bet(
            match_id=10,
            team="radiant",
            automatic=True,
            wagered=10,
            profit=-10,
            won=False,
            target_id=9001,
            direction="long",
        ),
        _history_bet(
            match_id=10,
            team="radiant",
            automatic=True,
            wagered=15,
            profit=-15,
            won=False,
            target_id=9002,
            direction="short",
        ),
        _history_bet(
            match_id=10,
            team="dire",
            automatic=False,
            wagered=30,
            profit=18,
            won=True,
        ),
    ]

    arbitrage = _calculate_auto_bet_performance(history).arbitrage

    assert arbitrage.bet_count == 1
    assert arbitrage.wins == 1
    assert arbitrage.total_wagered == 30
    assert arbitrage.net_pnl == 18


def test_same_side_manual_bet_and_unpaired_opposite_bet_are_not_arbitrage():
    history = [
        _history_bet(
            match_id=20,
            team="radiant",
            automatic=True,
            wagered=10,
            profit=7,
            won=True,
        ),
        _history_bet(
            match_id=20,
            team="radiant",
            automatic=False,
            wagered=5,
            profit=4,
            won=True,
        ),
        _history_bet(
            match_id=21,
            team="dire",
            automatic=False,
            wagered=8,
            profit=-8,
            won=False,
        ),
    ]

    arbitrage = _calculate_auto_bet_performance(history).arbitrage

    assert arbitrage.bet_count == 0
    assert arbitrage.wins == 0
    assert arbitrage.total_wagered == 0
    assert arbitrage.net_pnl == 0
