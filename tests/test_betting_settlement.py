import time

import pytest

from config import BOMB_POT_PARTICIPATION_BONUS, JOPACOIN_PER_GAME
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def services(repo_db_path):
    """Create test services using centralized fast fixture."""
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

    yield {
        "match_service": match_service,
        "betting_service": betting_service,
        "player_repo": player_repo,
        "db_path": repo_db_path,
    }


def test_settle_bets_pays_out_on_house(services):
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    player_ids = list(range(1000, 1010))
    # Add all players to database before shuffling
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    participant = pending["radiant_team_ids"][0]
    player_repo.add_balance(participant, TEST_GUILD_ID, 20)

    # Ensure betting is still open
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600  # 10 minutes in the future

    betting_service.place_bet(TEST_GUILD_ID, participant, "radiant", 5, pending)
    distributions = betting_service.settle_bets(123, TEST_GUILD_ID, "radiant", pending_state=pending)
    assert distributions, "Winning bet should appear in distributions"
    assert distributions["winners"][0]["discord_id"] == participant
    # Starting balance is now 3, plus 20, minus 5 bet, plus 10 payout = 28
    assert player_repo.get_balance(participant, TEST_GUILD_ID) == 28


def test_refund_pending_bets_on_abort(services):
    """Refunds should return coins and clear pending wagers when a match is aborted."""
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    player_ids = list(range(8200, 8210))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

    spectator = 8300
    player_repo.add(
        discord_id=spectator,
        discord_username="AbortSpectator",
        dotabuff_url="https://dotabuff.com/players/8300",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_balance(spectator, TEST_GUILD_ID, 12)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600

    betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 7, pending)
    # Starting balance 3 + 12 top-up - 7 bet = 8 remaining
    assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 8

    refunded = betting_service.refund_pending_bets(TEST_GUILD_ID, pending)
    assert refunded == 1
    # Refund restores to starting balance (3) + 12 top-up = 15
    assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 15
    assert betting_service.get_pending_bet(1, spectator, pending_state=pending) is None
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending)
    assert totals["radiant"] == 0 and totals["dire"] == 0


class TestPoolBettingSettlement:
    """Tests for pool (parimutuel) betting settlement."""

    def test_pool_betting_proportional_payout(self, services):
        """Pool mode: winners split the total pool proportionally."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(9000, 9010))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        # Create spectators
        spectator1 = 9100
        spectator2 = 9101
        spectator3 = 9102
        for spec_id in [spectator1, spectator2, spectator3]:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.add_balance(spec_id, TEST_GUILD_ID, 100)

        # Shuffle with pool mode
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert pending["betting_mode"] == "pool"

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        # Place bets: 100 on radiant (spectator1), 200 on dire (spectator2 + spectator3)
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 100, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 100, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator3, "dire", 100, pending)

        # Total pool = 300, Radiant pool = 100, Dire pool = 200
        # If Radiant wins: spectator1 gets 300 (3.0x)
        # If Dire wins: spectator2 and spectator3 each get 150 (1.5x)
        distributions = betting_service.settle_bets(200, TEST_GUILD_ID, "radiant", pending_state=pending)

        assert len(distributions["winners"]) == 1
        assert len(distributions["losers"]) == 2

        winner = distributions["winners"][0]
        assert winner["discord_id"] == spectator1
        assert winner["payout"] == 300  # Gets entire pool
        assert winner["multiplier"] == 3.0

        # Check balances: spectator1 started with 103, bet 100, won 300 = 303
        assert player_repo.get_balance(spectator1, TEST_GUILD_ID) == 303

    def test_pool_betting_multiple_winners_split(self, services):
        """Pool mode: multiple winners split proportionally."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(9200, 9210))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        spectator1 = 9300
        spectator2 = 9301
        spectator3 = 9302
        for spec_id in [spectator1, spectator2, spectator3]:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.add_balance(spec_id, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        # Place bets: 50 on radiant (spectator1), 50 on radiant (spectator2), 100 on dire (spectator3)
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 50, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "radiant", 50, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator3, "dire", 100, pending)

        # Total pool = 200, Radiant pool = 100
        # Radiant wins: each radiant bettor gets (their_bet / 100) * 200 = 2x
        distributions = betting_service.settle_bets(201, TEST_GUILD_ID, "radiant", pending_state=pending)

        assert len(distributions["winners"]) == 2
        assert len(distributions["losers"]) == 1

        for winner in distributions["winners"]:
            assert winner["payout"] == 100  # Each gets 50 * 2.0
            assert winner["multiplier"] == 2.0

    def test_pool_betting_no_winners_refunds_all(self, services):
        """Pool mode: if no bets on winning side, refund all bets."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(9400, 9410))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        spectator1 = 9500
        spectator2 = 9501
        for spec_id in [spectator1, spectator2]:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.add_balance(spec_id, TEST_GUILD_ID, 50)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        # Both bet on dire
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "dire", 30, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 20, pending)

        # Balances after betting: spectator1 = 53 - 30 = 23, spectator2 = 53 - 20 = 33
        assert player_repo.get_balance(spectator1, TEST_GUILD_ID) == 23
        assert player_repo.get_balance(spectator2, TEST_GUILD_ID) == 33

        # Radiant wins - no winners, should refund all
        distributions = betting_service.settle_bets(202, TEST_GUILD_ID, "radiant", pending_state=pending)

        assert len(distributions["winners"]) == 0
        assert len(distributions["losers"]) == 2

        # Check that all losers were refunded
        for loser in distributions["losers"]:
            assert loser.get("refunded") is True

        # Balances should be restored
        assert player_repo.get_balance(spectator1, TEST_GUILD_ID) == 53
        assert player_repo.get_balance(spectator2, TEST_GUILD_ID) == 53

    def test_house_mode_still_works(self, services):
        """House mode should still work when explicitly set."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(9600, 9610))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        spectator = 9700
        player_repo.add(
            discord_id=spectator,
            discord_username="HouseSpectator",
            dotabuff_url="https://dotabuff.com/players/9700",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 50)

        # Shuffle with house mode explicitly
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert pending["betting_mode"] == "house"

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 20, pending)

        # Balance: 53 (starting) - 20 (bet) = 33
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 33

        distributions = betting_service.settle_bets(203, TEST_GUILD_ID, "radiant", pending_state=pending)

        assert len(distributions["winners"]) == 1
        winner = distributions["winners"][0]
        assert winner["payout"] == 40  # 1:1 payout (bet * 2)
        assert "multiplier" not in winner  # House mode doesn't have multiplier

        # Balance: 33 + 40 = 73
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 73

    def test_pool_payouts_are_integers_no_fractional_coins(self, services):
        """Pool payouts must always be integers - no fractional jopacoins."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(9900, 9910))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        # Create 3 spectators with bets that would cause fractional division
        spectators = [9950, 9951, 9952]
        for spec_id in spectators:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.add_balance(spec_id, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Bets: 10 + 10 + 10 = 30 on radiant, 70 on dire (from a participant)
        # Total pool = 100
        # If radiant wins: each gets int(10/30 * 100) = int(33.33) = 33
        betting_service.place_bet(TEST_GUILD_ID, spectators[0], "radiant", 10, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectators[1], "radiant", 10, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectators[2], "radiant", 10, pending)

        # Add a dire bet to create a pool
        dire_bettor = pending["dire_team_ids"][0]
        player_repo.add_balance(dire_bettor, TEST_GUILD_ID, 100)
        betting_service.place_bet(TEST_GUILD_ID, dire_bettor, "dire", 70, pending)

        distributions = betting_service.settle_bets(300, TEST_GUILD_ID, "radiant", pending_state=pending)

        # Verify all payouts are integers
        for winner in distributions["winners"]:
            assert isinstance(winner["payout"], int), "Payout must be an integer"
            assert winner["payout"] == int(winner["payout"]), "No fractional coins"

        # Each winner bet 10 out of 30 radiant pool, total pool is 100
        # Payout = ceil(10/30 * 100) = ceil(33.33) = 34 each
        for winner in distributions["winners"]:
            assert winner["payout"] == 34

        # Note: 34*3 = 102, slightly more than the 100 total pool due to rounding up
        # This ensures winners never lose fractional coins
        total_paid = sum(w["payout"] for w in distributions["winners"])
        assert total_paid == 102

    def test_split_bets_no_rounding_exploit(self, services):
        """Splitting bets into many small wagers should not yield more than one equivalent bet.

        This prevents an exploit where placing 10x 1 JC bets at 5x leverage yields more
        than a single 10 JC bet at 5x leverage due to per-bet ceiling rounding.
        """
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(9800, 9810))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        # Two spectators: one places a single bet, one splits into many small bets
        single_bettor = 9850
        split_bettor = 9851
        opposing_bettor = 9852

        for spec_id in [single_bettor, split_bettor, opposing_bettor]:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.add_balance(spec_id, TEST_GUILD_ID, 1000)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Single bettor: one 50 JC bet (equivalent to 10x 5 JC)
        betting_service.place_bet(TEST_GUILD_ID, single_bettor, "radiant", 50, pending)

        # Split bettor: ten 5 JC bets (same total effective as single bettor)
        for _ in range(10):
            betting_service.place_bet(TEST_GUILD_ID, split_bettor, "radiant", 5, pending)

        # Opposing bettor to create odds
        betting_service.place_bet(TEST_GUILD_ID, opposing_bettor, "dire", 100, pending)

        distributions = betting_service.settle_bets(300, TEST_GUILD_ID, "radiant", pending_state=pending)

        # Group payouts by user
        payout_by_user = {}
        for winner in distributions["winners"]:
            uid = winner["discord_id"]
            payout_by_user[uid] = payout_by_user.get(uid, 0) + winner["payout"]

        single_payout = payout_by_user[single_bettor]
        split_payout = payout_by_user[split_bettor]

        # Key assertion: split bets should NOT yield more than a single equivalent bet
        # They should yield exactly the same (both have 50 JC effective, same multiplier)
        assert split_payout == single_payout, (
            f"Split bets yielded {split_payout} vs single bet {single_payout}. "
            f"Splitting should not be exploitable for extra coins."
        )


class TestMultipleBetsSettlement:
    """Settlement of multiple bets from the same user (house, pool, refund)."""

    def test_multiple_bets_settlement_house_mode(self, services):
        """Multiple bets from same user are all settled correctly in house mode."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(10400, 10410))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        spectator = 10500
        player_repo.add(
            discord_id=spectator,
            discord_username="HouseMultiBet",
            dotabuff_url="https://dotabuff.com/players/10500",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="house")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Place multiple bets: 10 at 1x, 10 at 2x
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=2)

        # Balance: 103 - 10 - 20 (10*2) = 73
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 73

        # Settle - radiant wins
        distributions = betting_service.settle_bets(400, TEST_GUILD_ID, "radiant", pending_state=pending)

        # Should have 2 winner entries for the same user
        assert len(distributions["winners"]) == 2
        total_payout = sum(w["payout"] for w in distributions["winners"])
        # First bet: 10 * 2 = 20, Second bet: 20 * 2 = 40, Total = 60
        assert total_payout == 60

        # Balance: 73 + 60 = 133
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 133

    def test_multiple_bets_settlement_pool_mode(self, services):
        """Multiple bets from same user are all settled correctly in pool mode."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(10600, 10610))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        spectator1 = 10700
        spectator2 = 10701
        for spec_id in [spectator1, spectator2]:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"PoolMultiBet{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.add_balance(spec_id, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Spectator1: 20 + 30 = 50 effective on radiant
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 20, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 30, pending)

        # Spectator2: 50 on dire
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 50, pending)

        # Total pool = 100, Radiant pool = 50, Dire pool = 50
        distributions = betting_service.settle_bets(401, TEST_GUILD_ID, "radiant", pending_state=pending)

        # Spectator1 has 2 entries, both win
        assert len(distributions["winners"]) == 2
        assert len(distributions["losers"]) == 1

        # Multiplier is 2.0 (100/50), each bet gets their share
        # 20 -> 40, 30 -> 60, total 100
        total_payout = sum(w["payout"] for w in distributions["winners"])
        assert total_payout == 100

    def test_multiple_bets_refund(self, services):
        """All bets from a user are refunded when match is aborted."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(10800, 10810))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        spectator = 10900
        player_repo.add(
            discord_id=spectator,
            discord_username="RefundMultiBet",
            dotabuff_url="https://dotabuff.com/players/10900",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Place multiple bets: 10 + 20 at 2x = 50 effective
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 20, pending, leverage=2)

        # Balance: 103 - 10 - 40 = 53
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 53

        # Refund all pending bets
        refunded_count = betting_service.refund_pending_bets(TEST_GUILD_ID, pending)
        assert refunded_count == 2

        # Balance restored: 53 + 10 + 40 = 103
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 103

        # No more pending bets
        bets = betting_service.get_pending_bets(TEST_GUILD_ID, spectator, pending_state=pending)
        assert len(bets) == 0


class TestBlindBetsSettlement:
    """Blind-bet amount calculations, settlement, and refunds."""

    def test_create_auto_blind_bets_rounding(self, services):
        """Verify round() behavior for 5% calculation."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12500, 12510))
        for i, pid in enumerate(player_ids):
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
            # Test various balances
            # 51: 5% = 2.55 -> rounds to 3
            # 50: 5% = 2.5 -> rounds to 2 (banker's rounding)
            # 54: 5% = 2.7 -> rounds to 3
            if i < 3:
                player_repo.add_balance(pid, TEST_GUILD_ID, 48)  # 51 total
            elif i < 6:
                player_repo.add_balance(pid, TEST_GUILD_ID, 47)  # 50 total
            else:
                player_repo.add_balance(pid, TEST_GUILD_ID, 51)  # 54 total

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # All should have blind bets (all >= 50)
        assert result["created"] == 10

        # Verify amounts based on rounding
        amounts = [b["amount"] for b in result["bets"]]
        # 51*0.05 = 2.55 -> 3 (3 players)
        # 50*0.05 = 2.5 -> 2 (3 players)
        # 54*0.05 = 2.7 -> 3 (4 players)
        assert amounts.count(3) == 7  # 3 + 4 players
        assert amounts.count(2) == 3  # 3 players with 50

    def test_blind_bet_settlement(self, services):
        """Blind bets settle correctly with manual bets."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12800, 12810))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
            player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Record initial balances (after blind bets)
        radiant_player = pending["radiant_team_ids"][0]

        # Create blind bets (5 jopacoin each, 25 per team)
        blind_result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )
        assert blind_result["total_radiant"] == 25
        assert blind_result["total_dire"] == 25

        # Check balance after blind bet (should be 95 = 100 - 5)
        assert player_repo.get_balance(radiant_player, TEST_GUILD_ID) == 95

        # Add a manual bet from radiant player (10 jopacoin)
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 10, pending)
        assert player_repo.get_balance(radiant_player, TEST_GUILD_ID) == 85

        # Settle - radiant wins
        # Total pool = 25 + 25 + 10 = 60
        # Radiant pool = 35 (25 blind + 10 manual)
        # Multiplier = 60/35 = 1.71
        distributions = betting_service.settle_bets(500, TEST_GUILD_ID, "radiant", pending_state=pending)

        # 5 radiant winners (blind) + 1 radiant winner (manual from same player who has 2 bets)
        assert len(distributions["winners"]) == 6  # 5 blind + 1 manual
        assert len(distributions["losers"]) == 5  # 5 dire blind bets

        # Check that radiant player got paid for both bets
        radiant_player_payouts = [
            w["payout"] for w in distributions["winners"]
            if w["discord_id"] == radiant_player
        ]
        assert len(radiant_player_payouts) == 2  # blind + manual

    def test_blind_bets_refunded_on_abort(self, services):
        """Blind bets are properly refunded when a match is aborted.

        Regression test: ensures blind bet coins are returned to players
        when the shuffle is aborted before the match is recorded.
        """
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(13300, 13310))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
            player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total

        # Record initial balances
        initial_balances = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in player_ids}
        assert all(b == 100 for b in initial_balances.values())

        # Shuffle and create blind bets
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending_state = match_service.get_last_shuffle(TEST_GUILD_ID)

        blind_result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending_state["radiant_team_ids"],
            dire_ids=pending_state["dire_team_ids"],
            shuffle_timestamp=pending_state["shuffle_timestamp"],
        )
        assert blind_result["created"] == 10

        # Verify balances decreased by 5% (5 jopacoin each)
        for pid in player_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == 95, f"Player {pid} should have 95 after blind bet"

        # Simulate abort: refund all pending bets
        refunded = betting_service.refund_pending_bets(TEST_GUILD_ID, pending_state)
        assert refunded == 10, "All 10 blind bets should be refunded"

        # Verify all balances restored
        for pid in player_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == 100, f"Player {pid} should have 100 after refund"

        # Verify no pending bets remain
        for pid in player_ids:
            bets = betting_service.get_pending_bets(TEST_GUILD_ID, pid, pending_state=pending_state)
            assert len(bets) == 0, f"Player {pid} should have no pending bets"

    def test_mixed_blind_and_manual_bets_refunded_on_abort(self, services):
        """Both blind bets and manual bets are refunded on abort."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(13400, 13410))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
            player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total

        # Add spectator who will place manual bet
        spectator = 13500
        player_repo.add(
            discord_id=spectator,
            discord_username="AbortSpectator",
            dotabuff_url="https://dotabuff.com/players/13500",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 47)  # 50 total

        # Shuffle and create blind bets
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending_state = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending_state["bet_lock_until"] = int(time.time()) + 600

        betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending_state["radiant_team_ids"],
            dire_ids=pending_state["dire_team_ids"],
            shuffle_timestamp=pending_state["shuffle_timestamp"],
        )

        # Spectator places manual bet
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 20, pending_state)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 30  # 50 - 20

        # Count total pending bets: 10 blind + 1 manual = 11
        all_bets = betting_service.get_all_pending_bets(TEST_GUILD_ID, pending_state)
        assert len(all_bets) == 11

        # Abort and refund
        refunded = betting_service.refund_pending_bets(TEST_GUILD_ID, pending_state)
        assert refunded == 11

        # Verify spectator balance restored
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 50

        # Verify player balances restored
        for pid in player_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == 100


class TestBombPotSettlement:
    """Bomb pot amount calculation and participation bonus payouts."""

    def test_bomb_pot_blind_bets_higher_percentage(self, services):
        """Bomb pot uses 10% instead of 5% for blind bets."""
        player_repo = services["player_repo"]
        betting_service = services["betting_service"]

        # Set up players with 100 balance each
        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]
        for pid in radiant_ids + dire_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=3000,
                glicko_rating=1500.0,
                guild_id=TEST_GUILD_ID,
            )
            player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total (3 default + 97)

        now_ts = int(time.time())

        # Normal mode: 5% of 100 = 5 JC per player
        normal_result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            shuffle_timestamp=now_ts,
            is_bomb_pot=False,
        )
        # 10 players * 5 JC = 50 total
        assert normal_result["total_radiant"] + normal_result["total_dire"] == 50

        # Reset balances for bomb pot test
        for pid in radiant_ids + dire_ids:
            current = player_repo.get_balance(pid, TEST_GUILD_ID)
            player_repo.add_balance(pid, TEST_GUILD_ID, 100 - current)

        # Bomb pot mode: 10% of 100 = 10 JC + 10 JC ante = 20 JC per player
        bomb_pot_result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            shuffle_timestamp=now_ts + 1,  # Different timestamp
            is_bomb_pot=True,
        )
        # 10 players * 20 JC = 200 total
        assert bomb_pot_result["total_radiant"] + bomb_pot_result["total_dire"] == 200
        assert bomb_pot_result["is_bomb_pot"] is True

    def test_bomb_pot_participation_bonus_losers(self, services):
        """Losers in bomb pot get base participation + bomb pot bonus."""
        player_repo = services["player_repo"]
        betting_service = services["betting_service"]

        losing_ids = [3001, 3002, 3003, 3004, 3005]
        for pid in losing_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                guild_id=TEST_GUILD_ID,
            )

        # Normal mode: losers get JOPACOIN_PER_GAME
        normal_result = betting_service.award_participation(losing_ids, TEST_GUILD_ID, is_bomb_pot=False)
        for pid in losing_ids:
            assert normal_result[pid]["net"] == JOPACOIN_PER_GAME
            assert normal_result[pid]["bomb_pot_bonus"] == 0

        # Bomb pot mode: losers get JOPACOIN_PER_GAME + bomb pot bonus
        bomb_pot_result = betting_service.award_participation(losing_ids, TEST_GUILD_ID, is_bomb_pot=True)
        for pid in losing_ids:
            assert bomb_pot_result[pid]["net"] == JOPACOIN_PER_GAME + BOMB_POT_PARTICIPATION_BONUS
            assert bomb_pot_result[pid]["bomb_pot_bonus"] == BOMB_POT_PARTICIPATION_BONUS

    def test_bomb_pot_participation_bonus_winners_only_bonus(self, services):
        """Winners in bomb pot get only the bomb pot bonus (not base participation)."""
        player_repo = services["player_repo"]
        betting_service = services["betting_service"]

        winning_ids = [4001, 4002, 4003, 4004, 4005]
        for pid in winning_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                guild_id=TEST_GUILD_ID,
            )

        # With bomb_pot_bonus_only=True, winners get only the bomb pot bonus
        result = betting_service.award_participation(
            winning_ids, TEST_GUILD_ID, is_bomb_pot=True, bomb_pot_bonus_only=True
        )
        for pid in winning_ids:
            assert result[pid]["net"] == BOMB_POT_PARTICIPATION_BONUS  # Only bomb pot bonus, no base
            assert result[pid]["bomb_pot_bonus"] == BOMB_POT_PARTICIPATION_BONUS

    def test_bomb_pot_bonus_only_no_bomb_pot_gives_nothing(self, services):
        """If bomb_pot_bonus_only but not bomb pot, give nothing."""
        player_repo = services["player_repo"]
        betting_service = services["betting_service"]

        player_ids = [5001, 5002]
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                guild_id=TEST_GUILD_ID,
            )

        # bomb_pot_bonus_only=True but is_bomb_pot=False should give 0
        result = betting_service.award_participation(
            player_ids, TEST_GUILD_ID, is_bomb_pot=False, bomb_pot_bonus_only=True
        )
        for pid in player_ids:
            assert result[pid]["net"] == 0
            assert result[pid]["bomb_pot_bonus"] == 0
