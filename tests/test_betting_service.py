import os
import tempfile
import time

import pytest

from database import Database
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository
from services.betting_service import BettingService
from services.match_service import MatchService


@pytest.fixture
def services():
    """Create test services with a temporary database."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    
    db = Database(db_path)
    player_repo = PlayerRepository(db_path)
    bet_repo = BetRepository(db_path)
    match_repo = MatchRepository(db_path)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True, betting_service=betting_service)
    
    yield {
        "match_service": match_service,
        "betting_service": betting_service,
        "player_repo": player_repo,
        "db_path": db_path,
    }
    
    # Cleanup
    try:
        os.unlink(db_path)
    except OSError:
        pass


def test_place_bet_requires_pending_state(services):
    with pytest.raises(ValueError, match="No pending match"):
        services["betting_service"].place_bet(1, 1001, "radiant", 5, None)


def test_bet_lock_enforced(services):
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    player_ids = list(range(1000, 1013))
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
        )
    player_repo.add_balance(1001, 10)
    match_service.shuffle_players(player_ids, guild_id=1)
    pending = match_service.get_last_shuffle(1)
    pending["bet_lock_until"] = int(time.time()) - 1

    with pytest.raises(ValueError, match="closed"):
        betting_service.place_bet(1, 1001, "radiant", 5, pending)


def test_participant_can_only_bet_on_own_team(services):
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
        )
    match_service.shuffle_players(player_ids, guild_id=1)
    pending = match_service.get_last_shuffle(1)
    participant = pending["radiant_team_ids"][0]
    spectator = 2000
    player_repo.add(
        discord_id=spectator,
        discord_username="Spectator",
        dotabuff_url="https://dotabuff.com/players/1",
    )
    player_repo.add_balance(participant, 20)
    player_repo.add_balance(spectator, 20)
    
    # Ensure betting is still open
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600  # 10 minutes in the future

    with pytest.raises(ValueError, match="Participants on Radiant"):
        betting_service.place_bet(1, participant, "dire", 5, pending)

    # Spectator can bet on either team
    betting_service.place_bet(1, spectator, "dire", 5, pending)
    
    # But cannot place a second bet
    with pytest.raises(ValueError, match="already have a bet"):
        betting_service.place_bet(1, spectator, "radiant", 5, pending)


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
        )
    match_service.shuffle_players(player_ids, guild_id=1)
    pending = match_service.get_last_shuffle(1)
    participant = pending["radiant_team_ids"][0]
    player_repo.add_balance(participant, 20)
    
    # Ensure betting is still open
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600  # 10 minutes in the future

    betting_service.place_bet(1, participant, "radiant", 5, pending)
    distributions = betting_service.settle_bets(123, 1, "radiant", pending_state=pending)
    assert distributions, "Winning bet should appear in distributions"
    assert distributions["winners"][0]["discord_id"] == participant
    # Starting balance is now 3, plus 20, minus 5 bet, plus 10 payout = 28
    assert player_repo.get_balance(participant) == 28


def test_betting_totals_only_include_pending_bets(services):
    """Verify that betting totals only count pending bets, not settled ones."""
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]
    # First match: place bets and settle them
    player_ids = list(range(3000, 3010))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    
    spectator1 = 4000
    spectator2 = 4001
    player_repo.add(
        discord_id=spectator1,
        discord_username="Spectator1",
        dotabuff_url="https://dotabuff.com/players/4000",
    )
    player_repo.add(
        discord_id=spectator2,
        discord_username="Spectator2",
        dotabuff_url="https://dotabuff.com/players/4001",
    )
    player_repo.add_balance(spectator1, 20)
    player_repo.add_balance(spectator2, 20)

    match_service.shuffle_players(player_ids, guild_id=1)
    pending1 = match_service.get_last_shuffle(1)
    
    # Ensure betting is still open
    if pending1.get("bet_lock_until") is None or pending1["bet_lock_until"] <= int(time.time()):
        pending1["bet_lock_until"] = int(time.time()) + 600

    # Place bets on first match: 3 on radiant, 2 on dire
    betting_service.place_bet(1, spectator1, "radiant", 3, pending1)
    betting_service.place_bet(1, spectator2, "dire", 2, pending1)
    
    # Verify totals show pending bets
    totals = betting_service.get_pot_odds(1, pending_state=pending1)
    assert totals["radiant"] == 3, "Should show 3 jopacoin on Radiant"
    assert totals["dire"] == 2, "Should show 2 jopacoin on Dire"
    
    # Settle the first match (assigns match_id to bets)
    betting_service.settle_bets(100, 1, "radiant", pending_state=pending1)
    
    # After settling, totals should be 0 (no pending bets)
    totals = betting_service.get_pot_odds(1, pending_state=pending1)
    assert totals["radiant"] == 0, "Should show 0 after settling (no pending bets)"
    assert totals["dire"] == 0, "Should show 0 after settling (no pending bets)"
    
    # Second match: place new bets
    player_ids2 = list(range(3010, 3020))
    for pid in player_ids2:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    
    spectator3 = 4002
    player_repo.add(
        discord_id=spectator3,
        discord_username="Spectator3",
        dotabuff_url="https://dotabuff.com/players/4002",
    )
    player_repo.add_balance(spectator3, 20)

    match_service.shuffle_players(player_ids2, guild_id=1)
    pending2 = match_service.get_last_shuffle(1)
    
    # Ensure betting is still open
    if pending2.get("bet_lock_until") is None or pending2["bet_lock_until"] <= int(time.time()):
        pending2["bet_lock_until"] = int(time.time()) + 600

    # Place bet on second match: 6 on dire
    betting_service.place_bet(1, spectator3, "dire", 6, pending2)
    
    # Verify totals only show the new pending bet, not the old settled ones
    totals = betting_service.get_pot_odds(1, pending_state=pending2)
    assert totals["radiant"] == 0, "Should show 0 on Radiant (no pending bets)"
    assert totals["dire"] == 6, "Should show 6 jopacoin on Dire (only pending bet)"


def test_stale_pending_bets_do_not_show_or_block_new_match(services):
    """Stale matchless bets (match_id NULL) from a prior shuffle should not leak."""
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    player_ids = list(range(8000, 8010))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

    spectator = 8100
    player_repo.add(
        discord_id=spectator,
        discord_username="Spectator8100",
        dotabuff_url="https://dotabuff.com/players/8100",
    )
    player_repo.add_balance(spectator, 50)

    # First shuffle + bet (will become stale)
    match_service.shuffle_players(player_ids, guild_id=1)
    pending_old = match_service.get_last_shuffle(1)
    if pending_old.get("bet_lock_until") is None or pending_old["bet_lock_until"] <= int(time.time()):
        pending_old["bet_lock_until"] = int(time.time()) + 600
    betting_service.place_bet(1, spectator, "radiant", 5, pending_old)

    # Wait to ensure a newer shuffle timestamp
    time.sleep(1)

    # New shuffle; old bet remains match_id NULL but should be ignored
    match_service.shuffle_players(player_ids, guild_id=1)
    pending_new = match_service.get_last_shuffle(1)
    if pending_new.get("bet_lock_until") is None or pending_new["bet_lock_until"] <= int(time.time()):
        pending_new["bet_lock_until"] = int(time.time()) + 600

    totals = betting_service.get_pot_odds(1, pending_state=pending_new)
    assert totals["radiant"] == 0 and totals["dire"] == 0, "Stale bets must not appear in new match totals"

    # Old bet should not block placing a new bet on the new match
    betting_service.place_bet(1, spectator, "dire", 4, pending_new)
    totals = betting_service.get_pot_odds(1, pending_state=pending_new)
    assert totals["radiant"] == 0
    assert totals["dire"] == 4


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
        )

    spectator = 8300
    player_repo.add(
        discord_id=spectator,
        discord_username="AbortSpectator",
        dotabuff_url="https://dotabuff.com/players/8300",
    )
    player_repo.add_balance(spectator, 12)

    match_service.shuffle_players(player_ids, guild_id=1)
    pending = match_service.get_last_shuffle(1)
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600

    betting_service.place_bet(1, spectator, "dire", 7, pending)
    # Starting balance 3 + 12 top-up - 7 bet = 8 remaining
    assert player_repo.get_balance(spectator) == 8

    refunded = betting_service.refund_pending_bets(1, pending)
    assert refunded == 1
    # Refund restores to starting balance (3) + 12 top-up = 15
    assert player_repo.get_balance(spectator) == 15
    assert betting_service.get_pending_bet(1, spectator, pending_state=pending) is None
    totals = betting_service.get_pot_odds(1, pending_state=pending)
    assert totals["radiant"] == 0 and totals["dire"] == 0


class TestPoolBetting:
    """Tests for pool (parimutuel) betting mode."""

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
            )
            player_repo.add_balance(spec_id, 100)

        # Shuffle with pool mode
        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="pool")
        pending = match_service.get_last_shuffle(1)
        assert pending["betting_mode"] == "pool"

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        # Place bets: 100 on radiant (spectator1), 200 on dire (spectator2 + spectator3)
        betting_service.place_bet(1, spectator1, "radiant", 100, pending)
        betting_service.place_bet(1, spectator2, "dire", 100, pending)
        betting_service.place_bet(1, spectator3, "dire", 100, pending)

        # Total pool = 300, Radiant pool = 100, Dire pool = 200
        # If Radiant wins: spectator1 gets 300 (3.0x)
        # If Dire wins: spectator2 and spectator3 each get 150 (1.5x)
        distributions = betting_service.settle_bets(200, 1, "radiant", pending_state=pending)

        assert len(distributions["winners"]) == 1
        assert len(distributions["losers"]) == 2

        winner = distributions["winners"][0]
        assert winner["discord_id"] == spectator1
        assert winner["payout"] == 300  # Gets entire pool
        assert winner["multiplier"] == 3.0

        # Check balances: spectator1 started with 103, bet 100, won 300 = 303
        assert player_repo.get_balance(spectator1) == 303

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
            )

        spectator1 = 9300
        spectator2 = 9301
        spectator3 = 9302
        for spec_id in [spectator1, spectator2, spectator3]:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
            )
            player_repo.add_balance(spec_id, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="pool")
        pending = match_service.get_last_shuffle(1)

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        # Place bets: 50 on radiant (spectator1), 50 on radiant (spectator2), 100 on dire (spectator3)
        betting_service.place_bet(1, spectator1, "radiant", 50, pending)
        betting_service.place_bet(1, spectator2, "radiant", 50, pending)
        betting_service.place_bet(1, spectator3, "dire", 100, pending)

        # Total pool = 200, Radiant pool = 100
        # Radiant wins: each radiant bettor gets (their_bet / 100) * 200 = 2x
        distributions = betting_service.settle_bets(201, 1, "radiant", pending_state=pending)

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
            )

        spectator1 = 9500
        spectator2 = 9501
        for spec_id in [spectator1, spectator2]:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
            )
            player_repo.add_balance(spec_id, 50)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="pool")
        pending = match_service.get_last_shuffle(1)

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        # Both bet on dire
        betting_service.place_bet(1, spectator1, "dire", 30, pending)
        betting_service.place_bet(1, spectator2, "dire", 20, pending)

        # Balances after betting: spectator1 = 53 - 30 = 23, spectator2 = 53 - 20 = 33
        assert player_repo.get_balance(spectator1) == 23
        assert player_repo.get_balance(spectator2) == 33

        # Radiant wins - no winners, should refund all
        distributions = betting_service.settle_bets(202, 1, "radiant", pending_state=pending)

        assert len(distributions["winners"]) == 0
        assert len(distributions["losers"]) == 2

        # Check that all losers were refunded
        for loser in distributions["losers"]:
            assert loser.get("refunded") is True

        # Balances should be restored
        assert player_repo.get_balance(spectator1) == 53
        assert player_repo.get_balance(spectator2) == 53

    def test_house_mode_still_works(self, services):
        """House mode (default) should still work as before."""
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
            )

        spectator = 9700
        player_repo.add(
            discord_id=spectator,
            discord_username="HouseSpectator",
            dotabuff_url="https://dotabuff.com/players/9700",
        )
        player_repo.add_balance(spectator, 50)

        # Shuffle with house mode (default)
        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)
        assert pending["betting_mode"] == "house"

        if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
            pending["bet_lock_until"] = int(time.time()) + 600

        betting_service.place_bet(1, spectator, "radiant", 20, pending)

        # Balance: 53 (starting) - 20 (bet) = 33
        assert player_repo.get_balance(spectator) == 33

        distributions = betting_service.settle_bets(203, 1, "radiant", pending_state=pending)

        assert len(distributions["winners"]) == 1
        winner = distributions["winners"][0]
        assert winner["payout"] == 40  # 1:1 payout (bet * 2)
        assert "multiplier" not in winner  # House mode doesn't have multiplier

        # Balance: 33 + 40 = 73
        assert player_repo.get_balance(spectator) == 73

    def test_shuffle_betting_mode_validation(self, services):
        """Invalid betting mode should raise an error."""
        match_service = services["match_service"]
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
            )

        with pytest.raises(ValueError, match="betting_mode must be"):
            match_service.shuffle_players(player_ids, guild_id=1, betting_mode="invalid")

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
            )

        # Create 3 spectators with bets that would cause fractional division
        spectators = [9950, 9951, 9952]
        for spec_id in spectators:
            player_repo.add(
                discord_id=spec_id,
                discord_username=f"Spectator{spec_id}",
                dotabuff_url=f"https://dotabuff.com/players/{spec_id}",
            )
            player_repo.add_balance(spec_id, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="pool")
        pending = match_service.get_last_shuffle(1)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Bets: 10 + 10 + 10 = 30 on radiant, 70 on dire (from a participant)
        # Total pool = 100
        # If radiant wins: each gets int(10/30 * 100) = int(33.33) = 33
        betting_service.place_bet(1, spectators[0], "radiant", 10, pending)
        betting_service.place_bet(1, spectators[1], "radiant", 10, pending)
        betting_service.place_bet(1, spectators[2], "radiant", 10, pending)

        # Add a dire bet to create a pool
        dire_bettor = pending["dire_team_ids"][0]
        player_repo.add_balance(dire_bettor, 100)
        betting_service.place_bet(1, dire_bettor, "dire", 70, pending)

        distributions = betting_service.settle_bets(300, 1, "radiant", pending_state=pending)

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
