import time

import pytest

from config import JOPACOIN_EXCLUSION_REWARD
from domain.models.pending_match_state import PendingMatchState
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
    if pending1.bet_lock_until is None or pending1.bet_lock_until <= int(time.time()):
        pending1.bet_lock_until = int(time.time()) + 600

    # Place bets on first match: 3 on radiant, 2 on dire
    betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 3, pending1)
    betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 2, pending1)

    # Verify totals show pending bets
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending1)
    assert totals["radiant"] == 3, "Should show 3 jopacoin on Radiant"
    assert totals["dire"] == 2, "Should show 2 jopacoin on Dire"

    # Settle the first match (assigns match_id to bets)
    betting_service.settle_bets(100, TEST_GUILD_ID, "radiant", pending_state=pending1)

    # Clear the pending match (simulates what record_match does)
    match_service.clear_last_shuffle(TEST_GUILD_ID, pending1.pending_match_id)

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
    if pending2.bet_lock_until is None or pending2.bet_lock_until <= int(time.time()):
        pending2.bet_lock_until = int(time.time()) + 600

    # Place bet on second match: 6 on dire
    betting_service.place_bet(TEST_GUILD_ID, spectator3, "dire", 6, pending2)

    # Verify totals only show the new pending bet, not the old settled ones
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending2)
    assert totals["radiant"] == 0, "Should show 0 on Radiant (no pending bets)"
    assert totals["dire"] == 6, "Should show 6 jopacoin on Dire (only pending bet)"


def test_stale_pending_bets_do_not_show_or_block_new_match(services, monkeypatch):
    """Stale matchless bets (match_id NULL) from a prior shuffle should not leak."""
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    # Monotonically advancing fake clock so the second shuffle gets a newer
    # timestamp without sleeping for real wall-clock time. Replaces the
    # banned time.sleep(1) below.
    fake_now = [int(time.time())]

    def _tick():
        fake_now[0] += 1
        return fake_now[0]

    monkeypatch.setattr(time, "time", _tick)

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
    if pending_old.bet_lock_until is None or pending_old.bet_lock_until <= int(
        time.time()
    ):
        pending_old.bet_lock_until = int(time.time()) + 600
    betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 5, pending_old)

    # No real sleep needed: the monkeypatched ``time.time`` advances on every
    # call, so the next shuffle picks up a strictly larger ``shuffle_timestamp``
    # without burning wall-clock time under ``pytest -n auto``.

    # Abort the first match (refund bets but don't settle)
    # This simulates the normal flow where a match must be completed or aborted before a new shuffle
    betting_service.refund_pending_bets(TEST_GUILD_ID, pending_old, pending_old.pending_match_id)
    match_service.clear_last_shuffle(TEST_GUILD_ID, pending_old.pending_match_id)

    # Now shuffle again with the same players
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending_new = match_service.get_last_shuffle(TEST_GUILD_ID)
    if pending_new.bet_lock_until is None or pending_new.bet_lock_until <= int(
        time.time()
    ):
        pending_new.bet_lock_until = int(time.time()) + 600

    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending_new)
    assert totals["radiant"] == 0 and totals["dire"] == 0, (
        "Stale bets must not appear in new match totals"
    )

    # Old bet should not block placing a new bet on the new match
    betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 4, pending_new)
    totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending_new)
    assert totals["radiant"] == 0
    assert totals["dire"] == 4


class TestBettingCore:
    """Core place_bet / get_pending_bets API tests."""

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
        pending.bet_lock_until = int(time.time()) + 600

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
        pending.bet_lock_until = int(time.time()) + 600

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


class TestBlindBetsCore:
    """Core blind-bet creation, flagging, and listing."""

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
            radiant_ids=pending.radiant_team_ids,
            dire_ids=pending.dire_team_ids,
            shuffle_timestamp=pending.shuffle_timestamp,
        )

        # All 10 players should have blind bets
        assert result["created"] == 10
        assert len(result["bets"]) == 10
        assert len(result["skipped"]) == 0

        # Each bet should be 10% of 100 = 10 jopacoin
        for bet in result["bets"]:
            assert bet["amount"] == 10

        # Totals should be even (5 players * 10 coins = 50 each side)
        assert result["total_radiant"] == 50
        assert result["total_dire"] == 50

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
        pending.bet_lock_until = int(time.time()) + 600

        # Create blind bets
        betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending.radiant_team_ids,
            dire_ids=pending.dire_team_ids,
            shuffle_timestamp=pending.shuffle_timestamp,
        )

        # Check that bets are marked as blind
        radiant_player = pending.radiant_team_ids[0]
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
        pending.bet_lock_until = int(time.time()) + 600

        # Create blind bets
        betting_service.create_auto_blind_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending.radiant_team_ids,
            dire_ids=pending.dire_team_ids,
            shuffle_timestamp=pending.shuffle_timestamp,
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
        assert pending.radiant_team_ids, "pending state must have radiant_team_ids"
        assert pending.dire_team_ids, "pending state must have dire_team_ids"
        assert pending.shuffle_timestamp is not None, "pending state must have shuffle_timestamp"

        # Verify they're actually lists of ints
        assert isinstance(pending.radiant_team_ids, list)
        assert isinstance(pending.dire_team_ids, list)
        assert len(pending.radiant_team_ids) == 5
        assert len(pending.dire_team_ids) == 5
        assert all(isinstance(x, int) for x in pending.radiant_team_ids)

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
        match_service.shuffle_players(player_ids, guild_id=guild_id, betting_mode=mode)

        # Step 2: Get pending state for blind bets (like commands/match.py line 205)
        # This is the CORRECT way - must use get_last_shuffle, not result
        pending_state = match_service.get_last_shuffle(guild_id)

        # Step 3: Create blind bets (like commands/match.py line 206-211)
        blind_bets_result = betting_service.create_auto_blind_bets(
            guild_id=guild_id,
            radiant_ids=pending_state.radiant_team_ids,
            dire_ids=pending_state.dire_team_ids,
            shuffle_timestamp=pending_state.shuffle_timestamp,
        )

        # Verify blind bets were created successfully
        assert blind_bets_result["created"] == 10
        assert blind_bets_result["total_radiant"] == 50  # 5 players * 10 coins
        assert blind_bets_result["total_dire"] == 50


class TestAutoSpectatorBets:
    """Rich spectator auto-wagers."""

    def test_create_auto_spectator_bets_uses_top_richest_spectators(self, services):
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        radiant_ids = list(range(15000, 15005))
        dire_ids = list(range(15005, 15010))
        for pid in radiant_ids + dire_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.update_balance(pid, TEST_GUILD_ID, 1000)

        spectator_balances = {
            15100: 500,
            15101: 400,
            15102: 300,
            15103: 200,
            15104: 100,
            15105: 90,
            15106: 80,
        }
        for pid, balance in spectator_balances.items():
            player_repo.add(
                discord_id=pid,
                discord_username=f"Spectator{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.update_balance(pid, TEST_GUILD_ID, balance)

        now_ts = int(time.time())
        result = betting_service.create_auto_spectator_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            shuffle_timestamp=now_ts,
        )

        assert result["created"] == 5
        bet_ids = {bet["discord_id"] for bet in result["bets"]}
        assert bet_ids == {15100, 15101, 15102, 15103, 15104}
        assert bet_ids.isdisjoint(radiant_ids + dire_ids)
        assert {bet["amount"] for bet in result["bets"]} == {5, 4, 3, 2, 1}
        assert abs(result["total_radiant"] - result["total_dire"]) <= 2

        for bet in result["bets"]:
            pid = bet["discord_id"]
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == spectator_balances[pid] - bet["amount"]

        pending = PendingMatchState(shuffle_timestamp=now_ts)
        assert betting_service.get_top_voluntary_bettor(TEST_GUILD_ID, pending_state=pending) is None

        placed_bets = betting_service.bet_repo.get_bets_for_pending_match(TEST_GUILD_ID, since_ts=now_ts)
        assert len(placed_bets) == 5
        assert all(bet["is_blind"] for bet in placed_bets)

    def test_auto_spectator_can_manual_bet_opposite_team_on_shuffle(self, services):
        """After auto-wager, shuffle spectator can /bet on the other team."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        player_ids = list(range(15200, 15210))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                guild_id=TEST_GUILD_ID,
            )
            player_repo.update_balance(pid, TEST_GUILD_ID, 50)

        spectator = 15300
        player_repo.add(
            discord_id=spectator,
            discord_username="RichSpectator",
            dotabuff_url="https://dotabuff.com/players/15300",
            guild_id=TEST_GUILD_ID,
        )
        player_repo.update_balance(spectator, TEST_GUILD_ID, 5000)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending.bet_lock_until = int(time.time()) + 600

        auto_result = betting_service.create_auto_spectator_bets(
            guild_id=TEST_GUILD_ID,
            radiant_ids=pending.radiant_team_ids,
            dire_ids=pending.dire_team_ids,
            shuffle_timestamp=pending.shuffle_timestamp,
            pending_match_id=pending.pending_match_id,
        )
        assert auto_result["created"] == 1
        auto_team = auto_result["bets"][0]["team"]
        other_team = "dire" if auto_team == "radiant" else "radiant"

        betting_service.place_bet(TEST_GUILD_ID, spectator, other_team, 10, pending)

        bets = betting_service.get_pending_bets(TEST_GUILD_ID, spectator, pending_state=pending)
        assert {b["team_bet_on"] for b in bets} == {auto_team, other_team}


class TestPendingMatchPersistence:
    """Tests for pending_match payload persistence (flags, rating system)."""

    def test_bomb_pot_flag_persisted_in_pending_match(self, services):
        """Verify is_bomb_pot flag is included in persisted pending match payload."""
        from domain.models.pending_match_state import PendingMatchState
        match_service = services["match_service"]

        # Create a mock pending state with is_bomb_pot=True
        pending_state = PendingMatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=7500.0,
            dire_value=7500.0,
            value_diff=0.0,
            first_pick_team="radiant",
            shuffle_timestamp=int(time.time()),
            bet_lock_until=int(time.time()) + 900,
            betting_mode="pool",
            is_bomb_pot=True,
        )

        # Build payload using the service's method
        payload = match_service._build_pending_match_payload(pending_state)

        # Verify is_bomb_pot is included
        assert "is_bomb_pot" in payload
        assert payload["is_bomb_pot"] is True

        # Also verify non-bomb-pot matches work
        pending_state.is_bomb_pot = False
        payload = match_service._build_pending_match_payload(pending_state)
        assert payload["is_bomb_pot"] is False

    def test_openskill_shuffle_flag_persisted_in_pending_match(self, services):
        """Verify is_openskill_shuffle flag is included in persisted pending match payload."""
        from domain.models.pending_match_state import PendingMatchState
        match_service = services["match_service"]

        pending_state = PendingMatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=7500.0,
            dire_value=7500.0,
            value_diff=0.0,
            first_pick_team="radiant",
            shuffle_timestamp=int(time.time()),
            bet_lock_until=int(time.time()) + 900,
            betting_mode="pool",
            is_openskill_shuffle=True,
        )

        payload = match_service._build_pending_match_payload(pending_state)
        assert "is_openskill_shuffle" in payload
        assert payload["is_openskill_shuffle"] is True

        # Non-openskill shuffle
        pending_state.is_openskill_shuffle = False
        payload = match_service._build_pending_match_payload(pending_state)
        assert payload["is_openskill_shuffle"] is False

    def test_balancing_rating_system_persisted_in_pending_match(self, services):
        """Verify balancing_rating_system is included in persisted pending match payload."""
        from domain.models.pending_match_state import PendingMatchState
        match_service = services["match_service"]

        pending_state = PendingMatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=7500.0,
            dire_value=7500.0,
            value_diff=0.0,
            first_pick_team="radiant",
            shuffle_timestamp=int(time.time()),
            bet_lock_until=int(time.time()) + 900,
            betting_mode="pool",
            balancing_rating_system="openskill",
        )

        payload = match_service._build_pending_match_payload(pending_state)
        assert "balancing_rating_system" in payload
        assert payload["balancing_rating_system"] == "openskill"

        # Glicko system
        pending_state.balancing_rating_system = "glicko"
        payload = match_service._build_pending_match_payload(pending_state)
        assert payload["balancing_rating_system"] == "glicko"

        # Jopacoin system
        pending_state.balancing_rating_system = "jopacoin"
        payload = match_service._build_pending_match_payload(pending_state)
        assert payload["balancing_rating_system"] == "jopacoin"


def test_blessing_bonus_survives_sanctuary_exception(services):
    """A consumed Communion Blessing must still pay out if the sanctuary
    buff check raises.

    consume_and_credit_atomic destroys the one-shot blessing charge and
    credits the payout in the same transaction; an unrelated exception in
    the sanctuary check must not prevent the consumed charge from being
    reflected in the results bookkeeping.
    """
    from unittest.mock import MagicMock

    from config import JOPACOIN_WIN_REWARD

    betting_service = services["betting_service"]
    player_repo = services["player_repo"]

    pid = 7171
    player_repo.add(
        discord_id=pid,
        discord_username="BlessedWinner",
        guild_id=TEST_GUILD_ID,
        initial_mmr=1500,
    )
    player_repo.update_balance(pid, TEST_GUILD_ID, 0)

    def fake_consume_and_credit(buff_id, discord_id, guild_id, amount):
        # Mimic the real atomic repo method: consume succeeds and the
        # credit commits with it.
        player_repo.add_balance(discord_id, guild_id, amount)
        return True

    buff_service = MagicMock()
    buff_service.has_sanctuary_match_bonus.side_effect = RuntimeError("boom")
    buff_service.buff_repo.active_for.return_value = [{"id": 1}]
    buff_service.buff_repo.consume_and_credit_atomic.side_effect = fake_consume_and_credit
    buff_service.apply_blood_pact_skim.return_value = 0
    betting_service.buff_service = buff_service

    results = betting_service.award_win_bonus([pid], TEST_GUILD_ID)

    blessing_bonus = max(1, int(JOPACOIN_WIN_REWARD * 0.10))
    assert buff_service.buff_repo.consume_and_credit_atomic.called, (
        "Precondition: the blessing charge must actually be consumed"
    )
    assert results[pid]["manashop_bonus"] == blessing_bonus, (
        "Consumed blessing charge must be paid even when the sanctuary check raises"
    )
    assert (
        player_repo.get_balance(pid, TEST_GUILD_ID)
        == JOPACOIN_WIN_REWARD + blessing_bonus
    )


def test_consume_and_credit_atomic_pays_once(repo_db_path):
    """The blessing charge pays exactly once under concurrent-style double
    consumption.

    The first consume_and_credit_atomic call claims the charge and credits
    the balance in the same transaction; a second call against the already
    consumed row must return False and leave the balance untouched (no
    double-pay, no burned charge without payout).
    """
    from repositories.buff_repository import BuffRepository
    from services.buff_service import BUFF_COMMUNION_BLESSING

    player_repo = PlayerRepository(repo_db_path)
    buff_repo = BuffRepository(repo_db_path)

    pid = 7272
    bonus = 25
    player_repo.add(
        discord_id=pid,
        discord_username="DoubleConsumer",
        guild_id=TEST_GUILD_ID,
        initial_mmr=1500,
    )
    player_repo.update_balance(pid, TEST_GUILD_ID, 0)
    buff_id = buff_repo.grant(
        pid, TEST_GUILD_ID, BUFF_COMMUNION_BLESSING, int(time.time()) + 3600
    )

    assert buff_repo.consume_and_credit_atomic(buff_id, pid, TEST_GUILD_ID, bonus) is True
    assert player_repo.get_balance(pid, TEST_GUILD_ID) == bonus

    # Second (concurrent-style) caller loses the claim: no credit.
    assert buff_repo.consume_and_credit_atomic(buff_id, pid, TEST_GUILD_ID, bonus) is False
    assert player_repo.get_balance(pid, TEST_GUILD_ID) == bonus
