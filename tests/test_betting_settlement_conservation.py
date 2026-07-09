"""Settlement-level invariants against a REAL BetRepository + BettingService.

The existing ``test_betting_settlement.py`` exercises house/pool payout *amounts*
but never re-settles a window or checks that jopacoin is conserved end to end.
This module covers two settlement guarantees the SQL is responsible for:

1. Idempotency — ``settle_bets`` tags each bet with ``match_id``; a second call
   for the same shuffle window finds only ``match_id IS NULL`` rows (none left)
   and so pays nobody a second time. A regression here double-credits winners.
2. Conservation — in pool mode the jopacoin paid back out is exactly the total
   pool (modulo per-user ceiling rounding, which can only ever round *up*); in
   house mode every winner's profit comes from the house at the configured
   multiplier and losers' debits exactly equal their stakes.

All tests use the centralized ``repo_db_path`` schema template, so the bet
settlement transaction runs against real SQLite, not a mock.
"""

import time

import pytest

from config import HOUSE_PAYOUT_MULTIPLIER
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def services(repo_db_path):
    """Real repos + services wired over the fast schema-template DB."""
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        betting_service=betting_service,
    )
    return {
        "match_service": match_service,
        "betting_service": betting_service,
        "player_repo": player_repo,
        "bet_repo": bet_repo,
    }


def _seed_players(player_repo, ids, balance=0):
    """Register players; optionally top up to ``balance`` above the 3 JC default."""
    for pid in ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        if balance:
            player_repo.add_balance(pid, TEST_GUILD_ID, balance)


def _open_shuffle(match_service, player_ids, mode="house"):
    """Shuffle ``player_ids`` and return a pending state with betting open."""
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode=mode)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending.bet_lock_until = int(time.time()) + 600
    return pending


class TestSettlementIdempotency:
    """Re-settling the same window must never pay a winner twice."""

    def test_house_mode_double_settle_pays_once(self, services):
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(20000, 20010))
        _seed_players(player_repo, player_ids)
        spectator = 20100
        _seed_players(player_repo, [spectator], balance=100)

        pending = _open_shuffle(match_service, player_ids, mode="house")
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 20, pending)
        # 103 starting - 20 staked
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 83

        first = betting_service.settle_bets(700, TEST_GUILD_ID, "radiant", pending_state=pending)
        assert len(first["winners"]) == 1
        # 1:1 house payout: 20 stake * (1 + multiplier) = 40
        assert first["winners"][0]["payout"] == 40
        balance_after_first = player_repo.get_balance(spectator, TEST_GUILD_ID)
        assert balance_after_first == 123  # 83 + 40

        # Re-settle the SAME window: bets are already tagged with match_id 700,
        # so the second call sees no match_id IS NULL rows and pays nothing.
        second = betting_service.settle_bets(700, TEST_GUILD_ID, "radiant", pending_state=pending)
        assert second == {"winners": [], "losers": []}
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == balance_after_first, (
            "Second settle of an already-settled window must not re-credit the winner"
        )

    def test_pool_mode_double_settle_pays_once(self, services):
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(21000, 21010))
        _seed_players(player_repo, player_ids)
        winner_spec, loser_spec = 21100, 21101
        _seed_players(player_repo, [winner_spec, loser_spec], balance=200)

        pending = _open_shuffle(match_service, player_ids, mode="pool")
        betting_service.place_bet(TEST_GUILD_ID, winner_spec, "radiant", 60, pending)
        betting_service.place_bet(TEST_GUILD_ID, loser_spec, "dire", 40, pending)

        first = betting_service.settle_bets(701, TEST_GUILD_ID, "radiant", pending_state=pending)
        assert len(first["winners"]) == 1
        winner_balance = player_repo.get_balance(winner_spec, TEST_GUILD_ID)

        second = betting_service.settle_bets(701, TEST_GUILD_ID, "radiant", pending_state=pending)
        assert second == {"winners": [], "losers": []}
        assert player_repo.get_balance(winner_spec, TEST_GUILD_ID) == winner_balance, (
            "Pool re-settle must not pay the winner the pool a second time"
        )

    def test_settle_after_refund_pays_nobody(self, services):
        """A refunded window is also tagged-or-cleared; settling it pays no one.

        Refund + later settle is the abort-then-finalize race. The refund debits
        the wager bets back; the subsequent settle must not also pay them.
        """
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(22000, 22010))
        _seed_players(player_repo, player_ids)
        spectator = 22100
        _seed_players(player_repo, [spectator], balance=100)

        pending = _open_shuffle(match_service, player_ids, mode="house")
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 25, pending)

        refunded = betting_service.refund_pending_bets(TEST_GUILD_ID, pending)
        assert refunded == 1
        balance_after_refund = player_repo.get_balance(spectator, TEST_GUILD_ID)
        assert balance_after_refund == 103  # fully restored

        distributions = betting_service.settle_bets(
            702, TEST_GUILD_ID, "radiant", pending_state=pending
        )
        assert distributions == {"winners": [], "losers": []}
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == balance_after_refund, (
            "Settling an already-refunded window must not pay the refunded bettor"
        )

    def test_settle_with_no_pending_state_is_noop(self, services):
        """settle_bets with pending_state lacking a timestamp returns empty.

        Guards against pulling stale wagers from a prior window when the service
        has no live pending state.
        """
        betting_service = services["betting_service"]
        from domain.models.pending_match_state import PendingMatchState

        stateless = PendingMatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=0.0,
            dire_value=0.0,
            value_diff=0.0,
            first_pick_team="radiant",
            shuffle_timestamp=None,
            bet_lock_until=None,
            betting_mode="house",
        )
        result = betting_service.settle_bets(703, TEST_GUILD_ID, "radiant", pending_state=stateless)
        assert result == {"winners": [], "losers": []}


class TestPayoutConservation:
    """Jopacoin in == jopacoin out (house bankroll / pool redistribution)."""

    def test_pool_payout_never_exceeds_pool_by_more_than_rounding(self, services):
        """Pool winners collectively receive the whole pool, +/- ceiling slack.

        Per-user ceiling rounding can only pad upward, and by at most 1 JC per
        winning user. So total_paid is in [total_pool, total_pool + n_winners].
        """
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(23000, 23010))
        _seed_players(player_repo, player_ids)
        # Three radiant winners with bets that force fractional shares, one dire loser.
        r1, r2, r3, d1 = 23100, 23101, 23102, 23103
        _seed_players(player_repo, [r1, r2, r3, d1], balance=500)

        pending = _open_shuffle(match_service, player_ids, mode="pool")
        betting_service.place_bet(TEST_GUILD_ID, r1, "radiant", 7, pending)
        betting_service.place_bet(TEST_GUILD_ID, r2, "radiant", 11, pending)
        betting_service.place_bet(TEST_GUILD_ID, r3, "radiant", 13, pending)
        betting_service.place_bet(TEST_GUILD_ID, d1, "dire", 50, pending)

        total_pool = 7 + 11 + 13 + 50
        distributions = betting_service.settle_bets(
            710, TEST_GUILD_ID, "radiant", pending_state=pending
        )

        winning_users = {w["discord_id"] for w in distributions["winners"]}
        total_paid = sum(w["payout"] for w in distributions["winners"])
        assert total_paid >= total_pool, "Winners must split at least the whole pool"
        assert total_paid <= total_pool + len(winning_users), (
            f"Pool payout {total_paid} overshot pool {total_pool} by more than "
            f"{len(winning_users)} JC of ceiling rounding"
        )
        # Losers in pool mode are not refunded (a winning side exists).
        assert all(not loser.get("refunded") for loser in distributions["losers"])

    def test_pool_winner_balances_reflect_distribution_exactly(self, services):
        """Each winner's credited balance == sum of their settled bet payouts."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(24000, 24010))
        _seed_players(player_repo, player_ids)
        r1, r2, d1 = 24100, 24101, 24102
        _seed_players(player_repo, [r1, r2, d1], balance=300)

        pending = _open_shuffle(match_service, player_ids, mode="pool")
        # r1 places two bets; r2 one; d1 loses.
        betting_service.place_bet(TEST_GUILD_ID, r1, "radiant", 20, pending)
        betting_service.place_bet(TEST_GUILD_ID, r1, "radiant", 10, pending)
        betting_service.place_bet(TEST_GUILD_ID, r2, "radiant", 30, pending)
        betting_service.place_bet(TEST_GUILD_ID, d1, "dire", 40, pending)

        # Balances after staking (303 start each).
        assert player_repo.get_balance(r1, TEST_GUILD_ID) == 273  # 303 - 30
        assert player_repo.get_balance(r2, TEST_GUILD_ID) == 273  # 303 - 30
        assert player_repo.get_balance(d1, TEST_GUILD_ID) == 263  # 303 - 40

        distributions = betting_service.settle_bets(
            711, TEST_GUILD_ID, "radiant", pending_state=pending
        )

        payout_by_user: dict[int, int] = {}
        for w in distributions["winners"]:
            payout_by_user[w["discord_id"]] = payout_by_user.get(w["discord_id"], 0) + w["payout"]

        # Credited balance == post-stake balance + summed payouts.
        assert player_repo.get_balance(r1, TEST_GUILD_ID) == 273 + payout_by_user[r1]
        assert player_repo.get_balance(r2, TEST_GUILD_ID) == 273 + payout_by_user[r2]
        # d1 lost: stake is gone, no payout, balance unchanged from post-stake.
        assert player_repo.get_balance(d1, TEST_GUILD_ID) == 263

    def test_house_mode_profit_comes_from_house_at_multiplier(self, services):
        """House winners net stake*multiplier profit; losers' debit == stake.

        With HOUSE_PAYOUT_MULTIPLIER=1.0 the payout is double the stake, i.e. a
        net profit of one stake. This asserts the house, not other bettors,
        funds the win — there is no pool to conserve.
        """
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(25000, 25010))
        _seed_players(player_repo, player_ids)
        winner_spec, loser_spec = 25100, 25101
        _seed_players(player_repo, [winner_spec, loser_spec], balance=100)

        pending = _open_shuffle(match_service, player_ids, mode="house")
        betting_service.place_bet(TEST_GUILD_ID, winner_spec, "radiant", 30, pending)
        betting_service.place_bet(TEST_GUILD_ID, loser_spec, "dire", 30, pending)

        distributions = betting_service.settle_bets(
            712, TEST_GUILD_ID, "radiant", pending_state=pending
        )
        assert len(distributions["winners"]) == 1
        assert len(distributions["losers"]) == 1

        expected_payout = int(30 * (1 + HOUSE_PAYOUT_MULTIPLIER))
        assert distributions["winners"][0]["payout"] == expected_payout
        # Winner: 103 start - 30 stake + payout.
        assert player_repo.get_balance(winner_spec, TEST_GUILD_ID) == 103 - 30 + expected_payout
        # Loser keeps the loss; house took the 30 stake, paid nothing back.
        assert player_repo.get_balance(loser_spec, TEST_GUILD_ID) == 103 - 30

    def test_house_leverage_payout_scales_with_effective_stake(self, services):
        """House payout uses effective stake (amount * leverage), not raw amount."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(26000, 26010))
        _seed_players(player_repo, player_ids)
        spectator = 26100
        _seed_players(player_repo, [spectator], balance=200)

        pending = _open_shuffle(match_service, player_ids, mode="house")
        # 10 JC at 3x leverage -> 30 JC effective debited.
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=3)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 203 - 30

        distributions = betting_service.settle_bets(
            713, TEST_GUILD_ID, "radiant", pending_state=pending
        )
        # Effective stake 30 -> payout 30 * (1 + multiplier).
        expected = int(30 * (1 + HOUSE_PAYOUT_MULTIPLIER))
        assert distributions["winners"][0]["payout"] == expected
        assert distributions["winners"][0]["effective_bet"] == 30
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 203 - 30 + expected

    def test_pool_no_winning_side_burns_effective_losing_stakes(self, services):
        """When nobody bet the winning side, losing effective stakes burn."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(27000, 27010))
        _seed_players(player_repo, player_ids)
        d1, d2 = 27100, 27101
        _seed_players(player_repo, [d1, d2], balance=200)

        pending = _open_shuffle(match_service, player_ids, mode="pool")
        # Both bet dire; d2 uses leverage so the burned amount is the effective stake.
        betting_service.place_bet(TEST_GUILD_ID, d1, "dire", 25, pending)
        betting_service.place_bet(TEST_GUILD_ID, d2, "dire", 10, pending, leverage=2)
        assert player_repo.get_balance(d1, TEST_GUILD_ID) == 203 - 25
        assert player_repo.get_balance(d2, TEST_GUILD_ID) == 203 - 20  # 10 * 2x

        # Radiant wins -> no real winners on radiant -> losing bets burn.
        distributions = betting_service.settle_bets(
            714, TEST_GUILD_ID, "radiant", pending_state=pending
        )
        assert distributions["winners"] == []
        assert len(distributions["losers"]) == 2
        assert all("refunded" not in loser for loser in distributions["losers"])
        assert player_repo.get_balance(d1, TEST_GUILD_ID) == 203 - 25
        assert player_repo.get_balance(d2, TEST_GUILD_ID) == 203 - 20
