"""
Tests for concurrent matches support.

Validates that multiple pending matches can exist in the same guild
with proper isolation of state, betting, and recording.
"""

import time

import pytest

from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def services(repo_db_path):
    """Create test services for concurrent match testing."""
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
        "match_repo": match_repo,
        "db_path": repo_db_path,
    }


def _register_players(player_repo, player_ids, guild_id=TEST_GUILD_ID, balance=100):
    """Helper to register players with balance."""
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
            guild_id=guild_id,
        )
        if balance > 0:
            player_repo.add_balance(pid, guild_id, balance)


def _create_two_concurrent_matches(services, players1, players2, guild_id=TEST_GUILD_ID):
    """
    Helper to create two concurrent matches and return their states.

    Returns:
        (state1, state2): Tuple of pending match states with pending_match_id
    """
    match_service = services["match_service"]
    match_repo = services["match_repo"]
    player_repo = services["player_repo"]

    # Register both sets of players
    _register_players(player_repo, players1, guild_id)
    _register_players(player_repo, players2, guild_id)

    # Create both shuffles
    match_service.shuffle_players(players1, guild_id=guild_id)
    match_service.shuffle_players(players2, guild_id=guild_id)

    # Get pending matches from DB (they have pending_match_id)
    pending_matches = match_repo.get_pending_matches(guild_id)

    # Match each state to the correct player set
    states = []
    for pm in pending_matches:
        pmid = pm["pending_match_id"]
        state = match_service.get_last_shuffle(guild_id, pending_match_id=pmid)
        states.append(state)

    # Return states in order: first contains players1, second contains players2
    state_players_0 = set(states[0]["radiant_team_ids"] + states[0]["dire_team_ids"])
    if state_players_0 == set(players1):
        return states[0], states[1]
    else:
        return states[1], states[0]


# =============================================================================
# CONCURRENT MATCH CREATION TESTS
# =============================================================================


class TestConcurrentMatchCreation:
    """Tests for creating multiple concurrent matches in the same guild."""

    def test_two_shuffles_create_distinct_pending_matches(self, services):
        """Two shuffles with different players create two separate pending matches."""
        match_service = services["match_service"]
        match_repo = services["match_repo"]
        player_repo = services["player_repo"]

        # Register 20 players (two sets of 10)
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        _register_players(player_repo, players_match1)
        _register_players(player_repo, players_match2)

        # Create first shuffle
        match_service.shuffle_players(players_match1, guild_id=TEST_GUILD_ID)

        # Create second shuffle
        match_service.shuffle_players(players_match2, guild_id=TEST_GUILD_ID)

        # Get all pending matches from DB
        pending_matches = match_repo.get_pending_matches(TEST_GUILD_ID)

        # Verify both have unique pending_match_ids
        assert len(pending_matches) == 2
        pmid1 = pending_matches[0]["pending_match_id"]
        pmid2 = pending_matches[1]["pending_match_id"]
        assert pmid1 is not None
        assert pmid2 is not None
        assert pmid1 != pmid2

    def test_get_all_pending_matches_returns_both(self, services):
        """get_all_pending_matches returns all concurrent matches."""
        match_service = services["match_service"]
        match_repo = services["match_repo"]
        player_repo = services["player_repo"]

        # Register 20 players
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        _register_players(player_repo, players_match1)
        _register_players(player_repo, players_match2)

        # Create two shuffles
        match_service.shuffle_players(players_match1, guild_id=TEST_GUILD_ID)
        match_service.shuffle_players(players_match2, guild_id=TEST_GUILD_ID)

        # Get all pending matches
        pending_matches = match_repo.get_pending_matches(TEST_GUILD_ID)

        assert len(pending_matches) == 2

    def test_get_last_shuffle_returns_none_with_multiple_matches(self, services):
        """get_last_shuffle returns None when multiple matches exist (forces explicit selection)."""
        match_service = services["match_service"]
        player_repo = services["player_repo"]

        # Register 20 players
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        _register_players(player_repo, players_match1)
        _register_players(player_repo, players_match2)

        # Create two shuffles
        match_service.shuffle_players(players_match1, guild_id=TEST_GUILD_ID)
        match_service.shuffle_players(players_match2, guild_id=TEST_GUILD_ID)

        # get_last_shuffle without pending_match_id should return None
        result = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert result is None

    def test_get_last_shuffle_with_specific_id_returns_correct_match(self, services):
        """get_last_shuffle with specific pending_match_id returns that match."""
        match_service = services["match_service"]
        match_repo = services["match_repo"]
        player_repo = services["player_repo"]

        # Register 20 players
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        _register_players(player_repo, players_match1)
        _register_players(player_repo, players_match2)

        # Create two shuffles
        match_service.shuffle_players(players_match1, guild_id=TEST_GUILD_ID)
        match_service.shuffle_players(players_match2, guild_id=TEST_GUILD_ID)

        # Get pending match IDs from DB
        pending_matches = match_repo.get_pending_matches(TEST_GUILD_ID)
        pmid1 = pending_matches[0]["pending_match_id"]
        pmid2 = pending_matches[1]["pending_match_id"]

        # Get specific matches
        state1 = match_service.get_last_shuffle(TEST_GUILD_ID, pending_match_id=pmid1)
        state2 = match_service.get_last_shuffle(TEST_GUILD_ID, pending_match_id=pmid2)

        assert state1 is not None
        assert state2 is not None
        # Match players to the correct state based on who's in each
        state1_players = set(state1["radiant_team_ids"] + state1["dire_team_ids"])
        state2_players = set(state2["radiant_team_ids"] + state2["dire_team_ids"])

        # One state should have match1 players, other should have match2 players
        assert (state1_players == set(players_match1) and state2_players == set(players_match2)) or \
               (state1_players == set(players_match2) and state2_players == set(players_match1))


# =============================================================================
# BETTING ISOLATION TESTS
# =============================================================================


class TestConcurrentMatchBettingIsolation:
    """Tests that betting is properly isolated between concurrent matches."""

    def test_get_pot_odds_isolated_between_matches(self, services):
        """get_pot_odds returns odds only for the specified match."""
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        # Create two concurrent matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        # Register spectators with balance
        spectator1, spectator2 = 3001, 3002
        _register_players(player_repo, [spectator1, spectator2])

        # Place bets on different matches
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 50, state1)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 30, state2)

        # Verify pot odds are isolated
        odds1 = betting_service.get_pot_odds(TEST_GUILD_ID, state1)
        odds2 = betting_service.get_pot_odds(TEST_GUILD_ID, state2)

        assert odds1["radiant"] == 50
        assert odds1["dire"] == 0
        assert odds2["radiant"] == 0
        assert odds2["dire"] == 30

    def test_get_all_pending_bets_isolated_between_matches(self, services):
        """get_all_pending_bets returns bets only for the specified match."""
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        # Create two concurrent matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        # Register spectators
        spectator1, spectator2 = 3001, 3002
        _register_players(player_repo, [spectator1, spectator2])

        # Place bets on different matches
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 50, state1)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 30, state2)

        # Verify pending bets are isolated
        bets1 = betting_service.get_all_pending_bets(TEST_GUILD_ID, state1)
        bets2 = betting_service.get_all_pending_bets(TEST_GUILD_ID, state2)

        assert len(bets1) == 1
        assert bets1[0]["discord_id"] == spectator1
        assert bets1[0]["team_bet_on"] == "radiant"

        assert len(bets2) == 1
        assert bets2[0]["discord_id"] == spectator2
        assert bets2[0]["team_bet_on"] == "dire"

    def test_spectator_can_bet_on_multiple_matches(self, services):
        """A spectator can place bets on multiple concurrent matches."""
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        # Create two concurrent matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        # Register spectator with enough balance for two bets
        spectator = 3001
        _register_players(player_repo, [spectator], balance=200)

        # Spectator bets on both matches
        betting_service.place_bet(TEST_GUILD_ID, spectator, "radiant", 50, state1)
        betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 50, state2)

        # Verify both bets exist
        bet1 = betting_service.get_pending_bet(TEST_GUILD_ID, spectator, state1)
        bet2 = betting_service.get_pending_bet(TEST_GUILD_ID, spectator, state2)

        assert bet1 is not None
        assert bet1["team_bet_on"] == "radiant"
        assert bet2 is not None
        assert bet2["team_bet_on"] == "dire"


# =============================================================================
# MATCH RECORDING ISOLATION TESTS
# =============================================================================


class TestConcurrentMatchRecordingIsolation:
    """Tests that recording one match doesn't affect other pending matches."""

    def test_record_match_1_preserves_match_2_state(self, services):
        """Recording Match #1 should not affect Match #2's pending state."""
        match_service = services["match_service"]

        # Create two concurrent matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        pmid1 = state1["pending_match_id"]
        pmid2 = state2["pending_match_id"]

        # Record match 1
        match_service.record_match(
            "radiant",
            guild_id=TEST_GUILD_ID,
            pending_match_id=pmid1,
        )

        # Verify match 2 is still pending
        state2_after = match_service.get_last_shuffle(TEST_GUILD_ID, pending_match_id=pmid2)
        assert state2_after is not None
        assert state2_after["pending_match_id"] == pmid2

        # Verify match 1 is gone
        state1_after = match_service.get_last_shuffle(TEST_GUILD_ID, pending_match_id=pmid1)
        assert state1_after is None

    def test_settle_bets_only_affects_target_match(self, services):
        """Settling bets for Match #1 should not touch Match #2's pending bets."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        # Create two concurrent matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        # Register spectators
        spectator1, spectator2 = 3001, 3002
        _register_players(player_repo, [spectator1, spectator2])

        # Place bets on both matches
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 50, state1)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 30, state2)

        # Record and settle match 1
        match_service.record_match(
            "radiant",
            guild_id=TEST_GUILD_ID,
            pending_match_id=state1["pending_match_id"],
        )

        # Verify match 2's bet is still pending (refresh state from DB)
        state2_fresh = match_service.get_last_shuffle(TEST_GUILD_ID, state2["pending_match_id"])
        bet2 = betting_service.get_pending_bet(TEST_GUILD_ID, spectator2, state2_fresh)
        assert bet2 is not None
        # Pending bets have match_id = None (not yet settled to a match)
        assert bet2["match_id"] is None

    def test_refund_bets_only_affects_aborted_match(self, services):
        """Aborting Match #1 only refunds Match #1's bets, not Match #2's."""
        match_service = services["match_service"]
        betting_service = services["betting_service"]
        player_repo = services["player_repo"]

        # Create two concurrent matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        # Register spectators
        spectator1, spectator2 = 3001, 3002
        _register_players(player_repo, [spectator1, spectator2])

        # Get initial balances
        initial_balance1 = player_repo.get_balance(spectator1, TEST_GUILD_ID)
        initial_balance2 = player_repo.get_balance(spectator2, TEST_GUILD_ID)

        # Place bets on both matches
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 50, state1)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 30, state2)

        # Refund match 1's bets (simulate abort)
        betting_service.refund_pending_bets(
            TEST_GUILD_ID, state1, pending_match_id=state1["pending_match_id"]
        )
        match_service.clear_last_shuffle(TEST_GUILD_ID, state1["pending_match_id"])

        # Spectator1 should be refunded
        final_balance1 = player_repo.get_balance(spectator1, TEST_GUILD_ID)
        assert final_balance1 == initial_balance1  # Full refund

        # Spectator2's bet should still be locked (not refunded)
        final_balance2 = player_repo.get_balance(spectator2, TEST_GUILD_ID)
        assert final_balance2 == initial_balance2 - 30  # Still deducted


# =============================================================================
# PLAYER CONSTRAINT TESTS
# =============================================================================


class TestConcurrentMatchPlayerConstraints:
    """Tests for player participation constraints across concurrent matches."""

    def test_get_all_pending_player_ids_returns_all_players(self, services):
        """get_all_pending_player_ids aggregates players from all concurrent matches."""
        match_service = services["match_service"]
        player_repo = services["player_repo"]

        # Register players for two matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        _register_players(player_repo, players_match1)
        _register_players(player_repo, players_match2)

        # Create two concurrent matches
        match_service.shuffle_players(players_match1, guild_id=TEST_GUILD_ID)
        match_service.shuffle_players(players_match2, guild_id=TEST_GUILD_ID)

        # Get all pending player IDs
        all_pending = match_service.state_service.get_all_pending_player_ids(TEST_GUILD_ID)

        assert len(all_pending) == 20
        assert set(players_match1).issubset(all_pending)
        assert set(players_match2).issubset(all_pending)

    def test_get_pending_match_for_player_finds_correct_match(self, services):
        """get_pending_match_for_player returns the specific match containing the player."""
        match_service = services["match_service"]

        # Create two concurrent matches using helper
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        # Lookup player from match 1
        player_in_match1 = players_match1[0]
        found = match_service.state_service.get_pending_match_for_player(TEST_GUILD_ID, player_in_match1)
        assert found is not None
        assert found["pending_match_id"] == state1["pending_match_id"]

        # Lookup player from match 2
        player_in_match2 = players_match2[0]
        found = match_service.state_service.get_pending_match_for_player(TEST_GUILD_ID, player_in_match2)
        assert found is not None
        assert found["pending_match_id"] == state2["pending_match_id"]

        # Lookup spectator (not in any match)
        spectator = 9999
        found = match_service.state_service.get_pending_match_for_player(TEST_GUILD_ID, spectator)
        assert found is None


# =============================================================================
# CLEAR/CLEANUP ISOLATION TESTS
# =============================================================================


class TestConcurrentMatchCleanupIsolation:
    """Tests that cleanup operations are properly isolated."""

    def test_clear_specific_match_preserves_others(self, services):
        """Clearing a specific match by ID doesn't affect other matches."""
        match_service = services["match_service"]

        # Create two concurrent matches using helper
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        state1, state2 = _create_two_concurrent_matches(services, players_match1, players_match2)

        pmid1 = state1["pending_match_id"]
        pmid2 = state2["pending_match_id"]

        # Clear only match 1
        match_service.clear_last_shuffle(TEST_GUILD_ID, pending_match_id=pmid1)

        # Match 1 should be gone
        state1_after = match_service.get_last_shuffle(TEST_GUILD_ID, pending_match_id=pmid1)
        assert state1_after is None

        # Match 2 should still exist
        state2_after = match_service.get_last_shuffle(TEST_GUILD_ID, pending_match_id=pmid2)
        assert state2_after is not None
        assert state2_after["pending_match_id"] == pmid2

    def test_clear_all_matches_removes_all(self, services):
        """Clearing without specific ID removes all pending matches."""
        match_service = services["match_service"]
        match_repo = services["match_repo"]
        player_repo = services["player_repo"]

        # Register players for two matches
        players_match1 = list(range(1000, 1010))
        players_match2 = list(range(2000, 2010))
        _register_players(player_repo, players_match1)
        _register_players(player_repo, players_match2)

        # Create two concurrent matches
        match_service.shuffle_players(players_match1, guild_id=TEST_GUILD_ID)
        match_service.shuffle_players(players_match2, guild_id=TEST_GUILD_ID)

        # Clear all matches
        match_service.clear_last_shuffle(TEST_GUILD_ID)

        # Both should be gone
        pending = match_repo.get_pending_matches(TEST_GUILD_ID)
        assert len(pending) == 0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
