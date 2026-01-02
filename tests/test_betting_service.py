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
