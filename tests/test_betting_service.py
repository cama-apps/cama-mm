import time

import pytest

from config import JOPACOIN_EXCLUSION_REWARD
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService


TEST_GUILD_ID = 12345


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
            guild_id=TEST_GUILD_ID,
        )
    player_repo.add_balance(1001, TEST_GUILD_ID, 10)
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    pending["bet_lock_until"] = int(time.time()) - 1

    with pytest.raises(ValueError, match="closed"):
        betting_service.place_bet(TEST_GUILD_ID, 1001, "radiant", 5, pending)


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
            guild_id=TEST_GUILD_ID,
        )
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    participant = pending["radiant_team_ids"][0]
    spectator = 2000
    player_repo.add(
        discord_id=spectator,
        discord_username="Spectator",
        dotabuff_url="https://dotabuff.com/players/1",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_balance(participant, TEST_GUILD_ID, 20)
    player_repo.add_balance(spectator, TEST_GUILD_ID, 20)

    # Ensure betting is still open
    if pending.get("bet_lock_until") is None or pending["bet_lock_until"] <= int(time.time()):
        pending["bet_lock_until"] = int(time.time()) + 600  # 10 minutes in the future

    with pytest.raises(ValueError, match="Participants on Radiant"):
        betting_service.place_bet(TEST_GUILD_ID, participant, "dire", 5, pending)

    # Spectator can bet on either team initially
    betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 5, pending)

    # Can add another bet on the same team
    betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 3, pending)

    # But cannot bet on the opposite team after betting
    with pytest.raises(ValueError, match="already have bets on Dire"):
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)


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


def test_award_exclusion_bonus_adds_reward(services):
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    pid = 7070
    player_repo.add(
        discord_id=pid,
        discord_username="ExcludedUser",
        dotabuff_url="https://dotabuff.com/players/7070",
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
    player_repo.update_balance(pid, TEST_GUILD_ID, 0)

    result = betting_service.award_exclusion_bonus([pid], TEST_GUILD_ID)

    assert result[pid]["gross"] == JOPACOIN_EXCLUSION_REWARD
    assert result[pid]["net"] == JOPACOIN_EXCLUSION_REWARD
    assert result[pid]["garnished"] == 0
    assert player_repo.get_balance(pid, TEST_GUILD_ID) == JOPACOIN_EXCLUSION_REWARD


def test_award_exclusion_bonus_empty_list_noop(services):
    betting_service = services["betting_service"]
    result = betting_service.award_exclusion_bonus([])
    assert result == {}


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
            guild_id=TEST_GUILD_ID,
        )

    spectator1 = 4000
    spectator2 = 4001
    player_repo.add(
        discord_id=spectator1,
        discord_username="Spectator1",
        dotabuff_url="https://dotabuff.com/players/4000",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add(
        discord_id=spectator2,
        discord_username="Spectator2",
        dotabuff_url="https://dotabuff.com/players/4001",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_balance(spectator1, TEST_GUILD_ID, 20)
    player_repo.add_balance(spectator2, TEST_GUILD_ID, 20)

    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending1 = match_service.get_last_shuffle(TEST_GUILD_ID)

    # Ensure betting is still open
    if pending1.get("bet_lock_until") is None or pending1["bet_lock_until"] <= int(time.time()):
        pending1["bet_lock_until"] = int(time.time()) + 600

    # Place bets on first match: 3 on radiant, 2 on dire
    betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 3, pending1)
    betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 2, pending1)

    # Verify totals show pending bets
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending1)
    assert totals["radiant"] == 3, "Should show 3 jopacoin on Radiant"
    assert totals["dire"] == 2, "Should show 2 jopacoin on Dire"

    # Settle the first match (assigns match_id to bets)
    betting_service.settle_bets(100, TEST_GUILD_ID, "radiant", pending_state=pending1)

    # After settling, totals should be 0 (no pending bets)
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending1)
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
            guild_id=TEST_GUILD_ID,
        )

    spectator3 = 4002
    player_repo.add(
        discord_id=spectator3,
        discord_username="Spectator3",
        dotabuff_url="https://dotabuff.com/players/4002",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_balance(spectator3, TEST_GUILD_ID, 20)

    match_service.shuffle_players(player_ids2, guild_id=TEST_GUILD_ID)
    pending2 = match_service.get_last_shuffle(TEST_GUILD_ID)

    # Ensure betting is still open
    if pending2.get("bet_lock_until") is None or pending2["bet_lock_until"] <= int(time.time()):
        pending2["bet_lock_until"] = int(time.time()) + 600

    # Place bet on second match: 6 on dire
    betting_service.place_bet(TEST_GUILD_ID, spectator3, "dire", 6, pending2)

    # Verify totals only show the new pending bet, not the old settled ones
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending2)
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
            guild_id=TEST_GUILD_ID,
        )

    spectator = 8100
    player_repo.add(
        discord_id=spectator,
        discord_username="Spectator8100",
        dotabuff_url="https://dotabuff.com/players/8100",
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_balance(spectator, TEST_GUILD_ID, 50)

    # First shuffle + bet (will become stale)
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending_old = match_service.get_last_shuffle(TEST_GUILD_ID)
    if pending_old.get("bet_lock_until") is None or pending_old["bet_lock_until"] <= int(
        time.time()
    ):
        pending_old["bet_lock_until"] = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending_old)

    # Wait to ensure a newer shuffle timestamp
    time.sleep(1)

    # New shuffle; old bet remains match_id NULL but should be ignored
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending_new = match_service.get_last_shuffle(TEST_GUILD_ID)
    if pending_new.get("bet_lock_until") is None or pending_new["bet_lock_until"] <= int(
        time.time()
    ):
        pending_new["bet_lock_until"] = int(time.time()) + 600

    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending_new)
    assert totals["radiant"] == 0 and totals["dire"] == 0, (
        "Stale bets must not appear in new match totals"
    )

    # Old bet should not block placing a new bet on the new match
    betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 4, pending_new)
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending_new)
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
            guild_id=TEST_GUILD_ID,
        )

        with pytest.raises(ValueError, match="betting_mode must be"):
            match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="invalid")

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


class TestMultipleBets:
    """Tests for placing multiple bets on the same team."""

    def test_can_place_multiple_bets_same_team(self, services):
        """User can place multiple bets on the same team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(10000, 10010))
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

        spectator = 10100
        player_repo.add(
            discord_id=spectator,
            discord_username="MultiBetSpectator",
            dotabuff_url="https://dotabuff.com/players/10100",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Place first bet
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending)
        # Place second bet on same team
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 15, pending)
        # Place third bet with leverage
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending, leverage=2)

        # Balance: 103 (starting) - 10 - 15 - 10 (5*2) = 68
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 68

        # Verify we can get all bets
        bets = betting_service.get_pending_bets(TEST_GUILD_ID, spectator, pending_state=pending)
        assert len(bets) == 3
        assert bets[0]["amount"] == 10
        assert bets[1]["amount"] == 15
        assert bets[2]["amount"] == 5
        assert bets[2]["leverage"] == 2

    def test_cannot_bet_opposite_team_after_betting(self, services):
        """Once bet on a team, cannot bet on the opposite team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(10200, 10210))
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

        spectator = 10300
        player_repo.add(
            discord_id=spectator,
            discord_username="OppositeTeamSpectator",
            dotabuff_url="https://dotabuff.com/players/10300",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Bet on radiant first
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending)

        # Try to bet on dire - should fail
        with pytest.raises(ValueError, match="already have bets on Radiant"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 10, pending)

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

    def test_get_pending_bets_returns_empty_when_none(self, services):
        """get_pending_bets returns empty list when user has no bets."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11000, 11010))
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

        spectator = 11100
        player_repo.add(
            discord_id=spectator,
            discord_username="NoBetsSpectator",
            dotabuff_url="https://dotabuff.com/players/11100",
        guild_id=TEST_GUILD_ID,
    )

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        # No bets placed - should return empty list
        bets = betting_service.get_pending_bets(TEST_GUILD_ID, spectator, pending_state=pending)
        assert bets == []

    def test_multiple_bets_with_different_leverage(self, services):
        """Bets with different leverage values are tracked correctly."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11200, 11210))
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

        spectator = 11300
        player_repo.add(
            discord_id=spectator,
            discord_username="MixedLeverageSpectator",
            dotabuff_url="https://dotabuff.com/players/11300",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 500)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Place bets with different leverage: 10@1x, 10@2x, 10@3x, 10@5x
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending)  # 10 effective
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=2)  # 20 effective
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=3)  # 30 effective
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=5)  # 50 effective

        # Balance: 503 - 10 - 20 - 30 - 50 = 393
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 393

        bets = betting_service.get_pending_bets(TEST_GUILD_ID, spectator, pending_state=pending)
        assert len(bets) == 4
        assert bets[0]["leverage"] == 1
        assert bets[1]["leverage"] == 2
        assert bets[2]["leverage"] == 3
        assert bets[3]["leverage"] == 5

        # Totals should reflect effective amounts
        totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending)
        assert totals["radiant"] == 110  # 10 + 20 + 30 + 50

    def test_multiple_bets_balance_enforced_each_bet(self, services):
        """Each bet checks balance independently, so multiple small bets can fail if balance runs out."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11400, 11410))
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

        spectator = 11500
        player_repo.add(
            discord_id=spectator,
            discord_username="LimitedBalanceSpectator",
            dotabuff_url="https://dotabuff.com/players/11500",
        guild_id=TEST_GUILD_ID,
    )
        # Only has 10 jopacoin (3 starting + 7 top-up)
        player_repo.add_balance(spectator, TEST_GUILD_ID, 7)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # First bet of 5 succeeds
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 5

        # Second bet of 3 succeeds
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 3, pending)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 2

        # Third bet of 5 fails (only 2 left)
        with pytest.raises(ValueError, match="Insufficient balance"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)

    def test_participant_can_place_multiple_bets_on_own_team(self, services):
        """Match participant can place multiple bets on their own team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11600, 11610))
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
            player_repo.add_balance(pid, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Get a participant from radiant team
        radiant_player = pending["radiant_team_ids"][0]

        # First bet on own team succeeds
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 10, pending)

        # Second bet on own team also succeeds
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 15, pending)

        # Third bet with leverage succeeds
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 5, pending, leverage=2)

        # Verify all bets recorded
        bets = betting_service.get_pending_bets(TEST_GUILD_ID, radiant_player, pending_state=pending)
        assert len(bets) == 3

        # Trying to bet on opposite team fails (participant restriction)
        with pytest.raises(ValueError, match="Participants on Radiant can only bet on Radiant"):
            betting_service.place_bet(TEST_GUILD_ID, radiant_player, "dire", 5, pending)

    def test_leverage_respects_max_debt(self, services):
        """Leverage bets cannot push you past MAX_DEBT."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11700, 11710))
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

        spectator = 11800
        player_repo.add(
            discord_id=spectator,
            discord_username="DebtSpectator",
            dotabuff_url="https://dotabuff.com/players/11800",
        guild_id=TEST_GUILD_ID,
    )
        # Start with 100 jopacoin (3 default + 97)
        player_repo.add_balance(spectator, TEST_GUILD_ID, 97)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Trying to bet 150 at 5x = 750 effective would go to -650 (past -500 MAX_DEBT)
        with pytest.raises(ValueError, match="exceed maximum debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 150, pending, leverage=5)

        # But 100 at 5x = 500 effective, goes to -400 (within limit)
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 100, pending, leverage=5)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == -400

        # Once in debt, cannot place any more bets
        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=2)

    def test_in_debt_user_cannot_place_any_bet(self, services):
        """User in debt cannot place any bets (1x or leverage)."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(11900, 11910))
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

        spectator = 12000
        player_repo.add(
            discord_id=spectator,
            discord_username="DebtNoBets",
            dotabuff_url="https://dotabuff.com/players/12000",
        guild_id=TEST_GUILD_ID,
    )
        # Put them in debt: start with 3, then go negative
        player_repo.add_balance(spectator, TEST_GUILD_ID, 47)  # Has 50

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Place leverage bet to go into debt
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 100, pending, leverage=5)
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == -450

        # Cannot place 1x bet while in debt
        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 1, pending)

        # Cannot place leverage bet while in debt either
        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 10, pending, leverage=5)

    def test_spectator_bet_then_opposite_team_blocked(self, services):
        """Spectator who bet on one team cannot switch to opposite team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12100, 12110))
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

        spectator = 12200
        player_repo.add(
            discord_id=spectator,
            discord_username="SwitchAttempt",
            dotabuff_url="https://dotabuff.com/players/12200",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Bet on dire first
        betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 10, pending)

        # Can add more to dire
        betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 15, pending)

        # Cannot switch to radiant
        with pytest.raises(ValueError, match="already have bets on Dire"):
            betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending)


class TestBlindBets:
    """Tests for auto-liquidity blind bets functionality."""

    def test_create_auto_blind_bets_basic(self, services):
        """Blind bets are created for all eligible players."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12300, 12310))
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
            # Give all players 100 jopacoin (above threshold of 50)
            player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 3 starting + 97 = 100

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # All 10 players should have blind bets
        assert result["created"] == 10
        assert len(result["bets"]) == 10
        assert len(result["skipped"]) == 0

        # Each bet should be 5% of 100 = 5 jopacoin
        for bet in result["bets"]:
            assert bet["amount"] == 5

        # Totals should be even (5 players * 5 coins = 25 each side)
        assert result["total_radiant"] == 25
        assert result["total_dire"] == 25

    def test_create_auto_blind_bets_threshold(self, services):
        """Players below threshold are skipped."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12400, 12410))
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
            # Alternate: some have 100, some have only 30 (below 50 threshold)
            if i % 2 == 0:
                player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total
            else:
                player_repo.add_balance(pid, TEST_GUILD_ID, 27)  # 30 total (below threshold)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # Only 5 players (those with 100) should have blind bets
        assert result["created"] == 5
        assert len(result["skipped"]) == 5

        # Check skipped reasons
        for skip in result["skipped"]:
            assert "threshold" in skip["reason"]

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

    def test_create_auto_blind_bets_in_debt(self, services):
        """Players in debt are skipped."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12600, 12610))
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
            if i < 5:
                player_repo.add_balance(pid, TEST_GUILD_ID, 97)  # 100 total
            else:
                # Put in debt
                player_repo.add_balance(pid, TEST_GUILD_ID, -103)  # -100 balance

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        result = betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # Only 5 non-debt players should have blind bets
        assert result["created"] == 5
        assert len(result["skipped"]) == 5

    def test_blind_bet_is_blind_flag(self, services):
        """Blind bets have is_blind flag set."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12700, 12710))
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
            player_repo.add_balance(pid, TEST_GUILD_ID, 97)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Create blind bets
        betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # Check that bets are marked as blind
        radiant_player = pending["radiant_team_ids"][0]
        bets = betting_service.get_pending_bets(TEST_GUILD_ID, radiant_player, pending_state=pending)
        assert len(bets) == 1
        assert bets[0]["is_blind"] == 1

        # Now add a manual bet
        betting_service.place_bet(TEST_GUILD_ID, radiant_player, "radiant", 10, pending)

        # Check both bets
        bets = betting_service.get_pending_bets(TEST_GUILD_ID, radiant_player, pending_state=pending)
        assert len(bets) == 2
        assert bets[0]["is_blind"] == 1  # First was blind
        assert bets[1]["is_blind"] == 0  # Second was manual

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
        dire_player = pending["dire_team_ids"][0]
        initial_radiant_balance = player_repo.get_balance(radiant_player, TEST_GUILD_ID)

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

    def test_get_all_pending_bets(self, services):
        """get_all_pending_bets returns all bets for /bets command."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(12900, 12910))
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
            player_repo.add_balance(pid, TEST_GUILD_ID, 97)

        # Add a spectator
        spectator = 13000
        player_repo.add(
            discord_id=spectator,
            discord_username="Spectator",
            dotabuff_url="https://dotabuff.com/players/13000",
        guild_id=TEST_GUILD_ID,
    )
        player_repo.add_balance(spectator, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending["bet_lock_until"] = int(time.time()) + 600

        # Create blind bets
        betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending["radiant_team_ids"],
            dire_ids=pending["dire_team_ids"],
            shuffle_timestamp=pending["shuffle_timestamp"],
        )

        # Add spectator bet
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 20, pending)

        # Get all pending bets
        all_bets = betting_service.get_all_pending_bets(TEST_GUILD_ID, pending_state=pending)

        # Should have 10 blind + 1 manual = 11 bets
        assert len(all_bets) == 11

        # Verify is_blind flag is present
        blind_bets = [b for b in all_bets if b.get("is_blind")]
        manual_bets = [b for b in all_bets if not b.get("is_blind")]
        assert len(blind_bets) == 10
        assert len(manual_bets) == 1

    def test_shuffle_result_vs_pending_state_keys(self, services):
        """Verify shuffle_players return vs pending state have different keys.

        This test documents that shuffle_players() return value does NOT contain
        radiant_team_ids/dire_team_ids/shuffle_timestamp - those are only in the
        pending state. Commands must use get_last_shuffle() to access these keys.

        Regression test for KeyError bug in commands/match.py blind bet creation.
        """
        match_service = services["match_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(13100, 13110))
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

        # shuffle_players returns a dict with team objects, not IDs
        result = match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")

        # These keys are NOT in the return value (they're Team objects instead)
        assert "radiant_team_ids" not in result, "shuffle_players should not return radiant_team_ids"
        assert "dire_team_ids" not in result, "shuffle_players should not return dire_team_ids"
        assert "shuffle_timestamp" not in result, "shuffle_players should not return shuffle_timestamp"

        # The return value has Team objects
        assert "radiant_team" in result
        assert "dire_team" in result

        # The pending state (from get_last_shuffle) HAS the IDs and timestamp
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert "radiant_team_ids" in pending, "pending state must have radiant_team_ids"
        assert "dire_team_ids" in pending, "pending state must have dire_team_ids"
        assert "shuffle_timestamp" in pending, "pending state must have shuffle_timestamp"

        # Verify they're actually lists of ints
        assert isinstance(pending["radiant_team_ids"], list)
        assert isinstance(pending["dire_team_ids"], list)
        assert len(pending["radiant_team_ids"]) == 5
        assert len(pending["dire_team_ids"]) == 5
        assert all(isinstance(x, int) for x in pending["radiant_team_ids"])

    def test_blind_bets_integration_like_shuffle_command(self, services):
        """Integration test that mimics commands/match.py shuffle flow.

        This test follows the exact pattern that the /shuffle command uses
        to create blind bets, ensuring the integration works correctly.
        """
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(13200, 13210))
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

        guild_id = TEST_GUILD_ID
        mode = "pool"

        # Step 1: Shuffle (like commands/match.py line 168)
        result = match_service.shuffle_players(player_ids, guild_id=guild_id, betting_mode=mode)

        # Step 2: Get pending state for blind bets (like commands/match.py line 205)
        # This is the CORRECT way - must use get_last_shuffle, not result
        pending_state = match_service.get_last_shuffle(guild_id)

        # Step 3: Create blind bets (like commands/match.py line 206-211)
        blind_bets_result = betting_service.create_auto_blind_bets(
            guild_id=guild_id,
            radiant_ids=pending_state["radiant_team_ids"],
            dire_ids=pending_state["dire_team_ids"],
            shuffle_timestamp=pending_state["shuffle_timestamp"],
        )

        # Verify blind bets were created successfully
        assert blind_bets_result["created"] == 10
        assert blind_bets_result["total_radiant"] == 25  # 5 players * 5 coins
        assert blind_bets_result["total_dire"] == 25

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
