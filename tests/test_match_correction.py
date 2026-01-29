"""
Tests for match correction functionality.

Tests the correct_match_result method that allows admins to fix
incorrectly recorded match results by reversing all effects and
re-applying with the correct winner.
"""

import os
import tempfile
import time

import pytest

from database import Database
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService


@pytest.fixture
def correction_services():
    """Create test services with a temporary database for correction tests."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)

    Database(db_path)
    player_repo = PlayerRepository(db_path)
    bet_repo = BetRepository(db_path)
    match_repo = MatchRepository(db_path)
    pairings_repo = PairingsRepository(db_path)
    betting_service = BettingService(bet_repo, player_repo)
    match_service = MatchService(
        player_repo=player_repo,
        match_repo=match_repo,
        use_glicko=True,
        betting_service=betting_service,
        pairings_repo=pairings_repo,
    )

    yield {
        "match_service": match_service,
        "betting_service": betting_service,
        "player_repo": player_repo,
        "match_repo": match_repo,
        "pairings_repo": pairings_repo,
        "bet_repo": bet_repo,
        "db_path": db_path,
    }

    # Cleanup
    try:
        os.unlink(db_path)
    except OSError:
        pass


def _create_players(player_repo, start_id=1000, count=10):
    """Helper to create test players."""
    player_ids = list(range(start_id, start_id + count))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
        # Give players some balance for betting
        player_repo.add_balance(pid, 100)
    return player_ids


class TestMatchCorrection:
    """Test suite for match result correction."""

    def test_correction_updates_win_loss_counters(self, correction_services):
        """Test that correcting a match swaps win/loss counters correctly."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo)
        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)

        radiant_ids = pending["radiant_team_ids"]
        dire_ids = pending["dire_team_ids"]

        # Record with Radiant winning (incorrectly)
        match_service.add_record_submission(1, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=1)
        match_id = result["match_id"]

        # Verify initial state: radiant won, dire lost
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid)
            assert player.wins == 1
            assert player.losses == 0

        for pid in dire_ids:
            player = player_repo.get_by_id(pid)
            assert player.wins == 0
            assert player.losses == 1

        # Correct to Dire winning
        correction_result = match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=1,
            corrected_by=99999,
        )

        assert correction_result["old_winning_team"] == "radiant"
        assert correction_result["new_winning_team"] == "dire"

        # Verify corrected state: dire won, radiant lost
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid)
            assert player.wins == 0, f"Player {pid} should have 0 wins after correction"
            assert player.losses == 1, f"Player {pid} should have 1 loss after correction"

        for pid in dire_ids:
            player = player_repo.get_by_id(pid)
            assert player.wins == 1, f"Player {pid} should have 1 win after correction"
            assert player.losses == 0, f"Player {pid} should have 0 losses after correction"

    def test_correction_updates_ratings(self, correction_services):
        """Test that ratings are recalculated correctly after correction."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=2000)
        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)

        radiant_ids = pending["radiant_team_ids"]
        dire_ids = pending["dire_team_ids"]

        # Store original ratings
        original_ratings = {}
        for pid in player_ids:
            rating_data = player_repo.get_glicko_rating(pid)
            original_ratings[pid] = rating_data[0] if rating_data else 1500.0

        # Record with Radiant winning
        match_service.add_record_submission(1, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=1)
        match_id = result["match_id"]

        # Get ratings after incorrect recording
        ratings_after_wrong = {}
        for pid in player_ids:
            rating_data = player_repo.get_glicko_rating(pid)
            ratings_after_wrong[pid] = rating_data[0] if rating_data else 1500.0

        # Radiant should have gained rating, Dire should have lost
        for pid in radiant_ids:
            assert ratings_after_wrong[pid] > original_ratings[pid], \
                "Radiant player should have gained rating from win"

        for pid in dire_ids:
            assert ratings_after_wrong[pid] < original_ratings[pid], \
                "Dire player should have lost rating from loss"

        # Correct to Dire winning
        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=1,
            corrected_by=99999,
        )

        # Get ratings after correction
        ratings_after_correction = {}
        for pid in player_ids:
            rating_data = player_repo.get_glicko_rating(pid)
            ratings_after_correction[pid] = rating_data[0] if rating_data else 1500.0

        # Now Dire should have gained, Radiant should have lost (relative to original)
        for pid in dire_ids:
            assert ratings_after_correction[pid] > original_ratings[pid], \
                "Dire player should have gained rating after correction"

        for pid in radiant_ids:
            assert ratings_after_correction[pid] < original_ratings[pid], \
                "Radiant player should have lost rating after correction"

    def test_correction_reverses_bet_payouts(self, correction_services):
        """Test that bet payouts are properly reversed and recalculated."""
        match_service = correction_services["match_service"]
        betting_service = correction_services["betting_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=3000)

        # Create a spectator who will bet
        spectator_id = 3999
        player_repo.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            dotabuff_url="https://dotabuff.com/players/3999",
            initial_mmr=1500,
        )
        player_repo.add_balance(spectator_id, 100)

        match_service.shuffle_players(player_ids, guild_id=1, betting_mode="pool")
        pending = match_service.get_last_shuffle(1)

        radiant_ids = pending["radiant_team_ids"]
        dire_ids = pending["dire_team_ids"]

        # Ensure betting is open
        pending["bet_lock_until"] = int(time.time()) + 600

        # Place bets: spectator bets on Dire
        betting_service.place_bet(1, spectator_id, "dire", 50, pending)

        # A radiant player bets on their own team
        betting_service.place_bet(1, radiant_ids[0], "radiant", 20, pending)

        # Record spectator balance before recording
        spectator_balance_before = player_repo.get_balance(spectator_id)

        # Record with Radiant winning (spectator loses their bet)
        match_service.add_record_submission(1, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=1)
        match_id = result["match_id"]

        # Spectator should have lost their bet (balance unchanged from bet deduction)
        spectator_balance_after_wrong = player_repo.get_balance(spectator_id)

        # Correct to Dire winning (spectator should now win their bet)
        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=1,
            corrected_by=99999,
        )

        spectator_balance_after_correction = player_repo.get_balance(spectator_id)

        # After correction, spectator (who bet on Dire) should have their payout
        # Their bet of 50 on Dire should now pay out
        assert spectator_balance_after_correction > spectator_balance_after_wrong, \
            "Spectator who bet on Dire should gain balance after correction to Dire win"

    def test_correction_updates_pairings(self, correction_services):
        """Test that pairings statistics are properly reversed and updated."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]
        pairings_repo = correction_services["pairings_repo"]

        player_ids = _create_players(player_repo, start_id=4000)
        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)

        radiant_ids = pending["radiant_team_ids"]
        dire_ids = pending["dire_team_ids"]

        # Record with Radiant winning
        match_service.add_record_submission(1, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=1)
        match_id = result["match_id"]

        # Check pairings: two radiant players should have wins_together=1
        p1, p2 = radiant_ids[0], radiant_ids[1]
        pair = pairings_repo.get_head_to_head(p1, p2)
        assert pair is not None, "Pairing should exist for radiant teammates"
        assert pair["games_together"] == 1
        assert pair["wins_together"] == 1

        # Two dire players should have games_together=1, wins_together=0
        d1, d2 = dire_ids[0], dire_ids[1]
        dpair = pairings_repo.get_head_to_head(d1, d2)
        assert dpair is not None, "Pairing should exist for dire teammates"
        assert dpair["games_together"] == 1
        assert dpair["wins_together"] == 0

        # Correct to Dire winning
        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=1,
            corrected_by=99999,
        )

        # After correction: radiant teammates should have 0 wins_together
        pair_after = pairings_repo.get_head_to_head(p1, p2)
        assert pair_after["games_together"] == 1
        assert pair_after["wins_together"] == 0, \
            "Radiant teammates should have 0 wins after correction"

        # Dire teammates should have 1 win_together
        dpair_after = pairings_repo.get_head_to_head(d1, d2)
        assert dpair_after["games_together"] == 1
        assert dpair_after["wins_together"] == 1, \
            "Dire teammates should have 1 win after correction"

    def test_correction_logs_audit_record(self, correction_services):
        """Test that corrections are logged for audit purposes."""
        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=5000)
        match_service.shuffle_players(player_ids, guild_id=1)

        match_service.add_record_submission(1, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=1)
        match_id = result["match_id"]

        admin_id = 88888
        correction_result = match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=1,
            corrected_by=admin_id,
        )

        assert correction_result["correction_id"] is not None

        # Check audit log
        corrections = match_repo.get_match_corrections(match_id)
        assert len(corrections) == 1
        assert corrections[0]["old_winning_team"] == 1  # Radiant
        assert corrections[0]["new_winning_team"] == 2  # Dire
        assert corrections[0]["corrected_by"] == admin_id

    def test_correction_rejects_same_result(self, correction_services):
        """Test that correcting to the same result raises an error."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=6000)
        match_service.shuffle_players(player_ids, guild_id=1)

        match_service.add_record_submission(1, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=1)
        match_id = result["match_id"]

        with pytest.raises(ValueError, match="already has radiant as winner"):
            match_service.correct_match_result(
                match_id=match_id,
                new_winning_team="radiant",
                guild_id=1,
            )

    def test_correction_rejects_nonexistent_match(self, correction_services):
        """Test that correcting a non-existent match raises an error."""
        match_service = correction_services["match_service"]

        with pytest.raises(ValueError, match="not found"):
            match_service.correct_match_result(
                match_id=99999,
                new_winning_team="dire",
                guild_id=1,
            )

    def test_double_correction_works(self, correction_services):
        """Test that correcting a match twice (back to original) works."""
        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=7000)
        match_service.shuffle_players(player_ids, guild_id=1)
        pending = match_service.get_last_shuffle(1)

        radiant_ids = pending["radiant_team_ids"]
        dire_ids = pending["dire_team_ids"]

        # Record with Radiant winning
        match_service.add_record_submission(1, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=1)
        match_id = result["match_id"]

        # Correct to Dire
        match_service.correct_match_result(match_id, "dire", 1, corrected_by=1)

        # Correct back to Radiant
        match_service.correct_match_result(match_id, "radiant", 1, corrected_by=1)

        # Verify final state matches original recording
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid)
            assert player.wins == 1
            assert player.losses == 0

        for pid in dire_ids:
            player = player_repo.get_by_id(pid)
            assert player.wins == 0
            assert player.losses == 1

        # Should have 2 correction records
        corrections = match_repo.get_match_corrections(match_id)
        assert len(corrections) == 2
