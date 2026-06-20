"""
Tests for match correction functionality.

Tests the correct_match_result method that allows admins to fix
incorrectly recorded match results by reversing all effects and
re-applying with the correct winner.
"""

import sqlite3
import time
from unittest import mock

import pytest

from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def correction_services(repo_db_path):
    """Create test services using centralized fast fixture."""
    player_repo = PlayerRepository(repo_db_path)
    bet_repo = BetRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    pairings_repo = PairingsRepository(repo_db_path)
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
        "db_path": repo_db_path,
    }


def _create_players(player_repo, start_id=1000, count=10):
    """Helper to create test players."""
    player_ids = list(range(start_id, start_id + count))
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
        # Give players some balance for betting
        player_repo.add_balance(pid, TEST_GUILD_ID, 100)
    return player_ids


class TestMatchCorrection:
    """Test suite for match result correction."""

    def test_correction_updates_win_loss_counters(self, correction_services):
        """Test that correcting a match swaps win/loss counters correctly."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        # Record with Radiant winning (incorrectly)
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Verify initial state: radiant won, dire lost
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1
            assert player.losses == 0

        for pid in dire_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 1

        # Correct to Dire winning
        correction_result = match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        assert correction_result["old_winning_team"] == "radiant"
        assert correction_result["new_winning_team"] == "dire"

        # Verify corrected state: dire won, radiant lost
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0, f"Player {pid} should have 0 wins after correction"
            assert player.losses == 1, f"Player {pid} should have 1 loss after correction"

        for pid in dire_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1, f"Player {pid} should have 1 win after correction"
            assert player.losses == 0, f"Player {pid} should have 0 losses after correction"

    def test_correction_updates_ratings(self, correction_services):
        """Test that ratings are recalculated correctly after correction."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=2000)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        # Store original ratings
        original_ratings = {}
        for pid in player_ids:
            rating_data = player_repo.get_glicko_rating(pid, TEST_GUILD_ID)
            original_ratings[pid] = rating_data[0] if rating_data else 1500.0

        # Record with Radiant winning
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Get ratings after incorrect recording
        ratings_after_wrong = {}
        for pid in player_ids:
            rating_data = player_repo.get_glicko_rating(pid, TEST_GUILD_ID)
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
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        # Get ratings after correction
        ratings_after_correction = {}
        for pid in player_ids:
            rating_data = player_repo.get_glicko_rating(pid, TEST_GUILD_ID)
            ratings_after_correction[pid] = rating_data[0] if rating_data else 1500.0

        # Now Dire should have gained, Radiant should have lost (relative to original)
        for pid in dire_ids:
            assert ratings_after_correction[pid] > original_ratings[pid], \
                "Dire player should have gained rating after correction"

        for pid in radiant_ids:
            assert ratings_after_correction[pid] < original_ratings[pid], \
                "Radiant player should have lost rating after correction"

    def test_correction_reverses_bet_payouts(self, correction_services):
        """Correction must credit/reverse the EXACT payouts, not merely move
        balances in the right direction. Cross-checks each player's balance
        against the payout independently stored in the bets table."""
        match_service = correction_services["match_service"]
        betting_service = correction_services["betting_service"]
        player_repo = correction_services["player_repo"]
        bet_repo = correction_services["bet_repo"]

        player_ids = _create_players(player_repo, start_id=3000)

        # Create a spectator who will bet
        spectator_id = 3999
        player_repo.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            guild_id=TEST_GUILD_ID,
            dotabuff_url="https://dotabuff.com/players/3999",
            initial_mmr=1500,
        )
        player_repo.add_balance(spectator_id, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        radiant_ids = pending.radiant_team_ids
        radiant_bettor = radiant_ids[0]

        # Ensure betting is open
        pending.bet_lock_until = int(time.time()) + 600

        # Place bets: spectator bets on Dire
        betting_service.place_bet(TEST_GUILD_ID, spectator_id, "dire", 50, pending)

        # A radiant player bets on their own team
        betting_service.place_bet(TEST_GUILD_ID, radiant_bettor, "radiant", 20, pending)

        # Record with Radiant winning (spectator loses, radiant bettor wins).
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Capture pre-correction balances and the payout the radiant bettor was
        # actually credited (stored on the bets table), so we can assert the
        # reversal subtracts EXACTLY that amount.
        spectator_balance_after_wrong = player_repo.get_balance(spectator_id, TEST_GUILD_ID)
        radiant_balance_after_wrong = player_repo.get_balance(radiant_bettor, TEST_GUILD_ID)
        bets_before = bet_repo.get_settled_bets_for_match(match_id)
        radiant_old_payout = next(
            b["payout"] for b in bets_before if b["discord_id"] == radiant_bettor
        )
        assert radiant_old_payout and radiant_old_payout > 0

        # Correct to Dire winning (spectator now wins, radiant bettor now loses).
        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        bets_after = bet_repo.get_settled_bets_for_match(match_id)
        spectator_new_payout = next(
            b["payout"] for b in bets_after if b["discord_id"] == spectator_id
        )
        radiant_bet_after = next(
            b for b in bets_after if b["discord_id"] == radiant_bettor
        )

        spectator_balance_after = player_repo.get_balance(spectator_id, TEST_GUILD_ID)
        radiant_balance_after = player_repo.get_balance(radiant_bettor, TEST_GUILD_ID)

        # New winner: credited EXACTLY the payout written to the bets table.
        assert spectator_new_payout is not None and spectator_new_payout > 0
        assert spectator_balance_after == spectator_balance_after_wrong + spectator_new_payout, \
            "Spectator must be credited exactly the new winning payout"

        # Former winner: payout column nulled and balance reduced by EXACTLY
        # the stale payout it previously held — no more, no less.
        assert radiant_bet_after["payout"] is None
        assert radiant_balance_after == radiant_balance_after_wrong - radiant_old_payout, \
            "Former winner must have exactly its old payout reversed"

    def test_correction_bet_settlement_is_all_or_nothing(self, correction_services):
        """If the player-balance credit fails mid-correction, the bets-table
        payout rewrite must roll back with it — never leave bets paid-out while
        balances were never credited (and vice versa).

        Pre-fix, the bets rewrite committed in its own transaction and the
        balance deltas were applied in a SEPARATE commit afterward, so a failure
        during the balance write stranded the new winners' payout (bets paid,
        balances not). The fix folds the balance UPDATE and the bets rewrite
        into one transaction. We inject a failure on the balance UPDATE itself
        (the `jopacoin_balance` write present in both designs): in the buggy
        design the bets rewrite has already committed, so this test catches the
        regression; in the fixed design the whole transaction rolls back."""
        match_service = correction_services["match_service"]
        betting_service = correction_services["betting_service"]
        player_repo = correction_services["player_repo"]
        bet_repo = correction_services["bet_repo"]

        player_ids = _create_players(player_repo, start_id=6000)
        spectator_id = 6999
        player_repo.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            guild_id=TEST_GUILD_ID,
            dotabuff_url="https://dotabuff.com/players/6999",
            initial_mmr=1500,
        )
        player_repo.add_balance(spectator_id, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        radiant_ids = pending.radiant_team_ids
        radiant_bettor = radiant_ids[0]
        pending.bet_lock_until = int(time.time()) + 600

        betting_service.place_bet(TEST_GUILD_ID, spectator_id, "dire", 50, pending)
        betting_service.place_bet(TEST_GUILD_ID, radiant_bettor, "radiant", 20, pending)

        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        spectator_balance_before = player_repo.get_balance(spectator_id, TEST_GUILD_ID)
        radiant_balance_before = player_repo.get_balance(radiant_bettor, TEST_GUILD_ID)
        bets_before = {b["discord_id"]: b["payout"] for b in bet_repo.get_settled_bets_for_match(match_id)}

        # Make any write to players.jopacoin_balance raise. This is the balance
        # credit step — inlined in the atomic block after the fix, and a
        # separate post-commit step before it. We patch get_connection on BOTH
        # repos involved in settlement so the failure is hit wherever the
        # balance write happens.
        def _guard(sql):
            if "jopacoin_balance" in sql and "UPDATE" in sql.upper():
                raise sqlite3.OperationalError("injected balance-write failure")

        class _CursorProxy:
            def __init__(self, cur):
                self._cur = cur

            def execute(self, sql, *a, **k):
                _guard(sql)
                return self._cur.execute(sql, *a, **k)

            def executemany(self, sql, *a, **k):
                _guard(sql)
                return self._cur.executemany(sql, *a, **k)

            def __getattr__(self, name):
                return getattr(self._cur, name)

            def __iter__(self):
                return iter(self._cur)

        class _ConnProxy:
            def __init__(self, conn):
                self._conn = conn

            def execute(self, sql, *a, **k):
                _guard(sql)
                return self._conn.execute(sql, *a, **k)

            def executemany(self, sql, *a, **k):
                _guard(sql)
                return self._conn.executemany(sql, *a, **k)

            def cursor(self):
                return _CursorProxy(self._conn.cursor())

            def __getattr__(self, name):
                return getattr(self._conn, name)

        def make_failing_get_connection(repo):
            real_get_conn = repo.get_connection

            def _get():
                return _ConnProxy(real_get_conn())
            return _get

        with mock.patch.object(
            bet_repo, "get_connection", make_failing_get_connection(bet_repo)
        ), mock.patch.object(
            player_repo, "get_connection", make_failing_get_connection(player_repo)
        ):
            with pytest.raises(sqlite3.OperationalError):
                match_service.correct_match_result(
                    match_id=match_id,
                    new_winning_team="dire",
                    guild_id=TEST_GUILD_ID,
                    corrected_by=99999,
                )

        # Nothing was half-applied: balances and bets.payout are unchanged.
        assert player_repo.get_balance(spectator_id, TEST_GUILD_ID) == spectator_balance_before
        assert player_repo.get_balance(radiant_bettor, TEST_GUILD_ID) == radiant_balance_before
        bets_after = {b["discord_id"]: b["payout"] for b in bet_repo.get_settled_bets_for_match(match_id)}
        assert bets_after == bets_before, \
            "Bets-table payouts must be untouched when the balance credit fails"

    def test_correction_clears_stale_payout_on_former_winner(self, correction_services):
        """A bet that won, then becomes a loser after correction, must have its
        payout column nulled — otherwise gambling stats stay permanently wrong."""
        match_service = correction_services["match_service"]
        betting_service = correction_services["betting_service"]
        player_repo = correction_services["player_repo"]
        bet_repo = correction_services["bet_repo"]

        player_ids = _create_players(player_repo, start_id=4000)

        spectator_id = 4999
        player_repo.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            guild_id=TEST_GUILD_ID,
            dotabuff_url="https://dotabuff.com/players/4999",
            initial_mmr=1500,
        )
        player_repo.add_balance(spectator_id, TEST_GUILD_ID, 100)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        radiant_ids = pending.radiant_team_ids
        pending.bet_lock_until = int(time.time()) + 600

        # Radiant bettor wins initially; spectator bets Dire and loses.
        betting_service.place_bet(TEST_GUILD_ID, radiant_ids[0], "radiant", 20, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator_id, "dire", 50, pending)

        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # The Radiant bettor's winning bet now holds a non-null payout.
        bets = bet_repo.get_settled_bets_for_match(match_id)
        radiant_bet = next(b for b in bets if b["discord_id"] == radiant_ids[0])
        assert radiant_bet["payout"] is not None

        # Correct to Dire winning: the Radiant bettor is now a loser.
        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        bets_after = bet_repo.get_settled_bets_for_match(match_id)
        radiant_bet_after = next(b for b in bets_after if b["discord_id"] == radiant_ids[0])
        assert radiant_bet_after["payout"] is None, \
            "Former winner that lost the correction must have a NULL payout"

    def test_correction_updates_pairings(self, correction_services):
        """Test that pairings statistics are properly reversed and updated."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]
        pairings_repo = correction_services["pairings_repo"]

        player_ids = _create_players(player_repo, start_id=4000)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        # Record with Radiant winning
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Check pairings: two radiant players should have wins_together=1
        p1, p2 = radiant_ids[0], radiant_ids[1]
        pair = pairings_repo.get_head_to_head(p1, p2, TEST_GUILD_ID)
        assert pair is not None, "Pairing should exist for radiant teammates"
        assert pair["games_together"] == 1
        assert pair["wins_together"] == 1

        # Two dire players should have games_together=1, wins_together=0
        d1, d2 = dire_ids[0], dire_ids[1]
        dpair = pairings_repo.get_head_to_head(d1, d2, TEST_GUILD_ID)
        assert dpair is not None, "Pairing should exist for dire teammates"
        assert dpair["games_together"] == 1
        assert dpair["wins_together"] == 0

        # Correct to Dire winning
        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        # After correction: radiant teammates should have 0 wins_together
        pair_after = pairings_repo.get_head_to_head(p1, p2, TEST_GUILD_ID)
        assert pair_after["games_together"] == 1
        assert pair_after["wins_together"] == 0, \
            "Radiant teammates should have 0 wins after correction"

        # Dire teammates should have 1 win_together
        dpair_after = pairings_repo.get_head_to_head(d1, d2, TEST_GUILD_ID)
        assert dpair_after["games_together"] == 1
        assert dpair_after["wins_together"] == 1, \
            "Dire teammates should have 1 win after correction"

    def test_correction_logs_audit_record(self, correction_services):
        """Test that corrections are logged for audit purposes."""
        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=5000)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        admin_id = 88888
        correction_result = match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
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
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)

        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        with pytest.raises(ValueError, match="already has radiant as winner"):
            match_service.correct_match_result(
                match_id=match_id,
                new_winning_team="radiant",
                guild_id=TEST_GUILD_ID,
            )

    def test_correction_rejects_nonexistent_match(self, correction_services):
        """Test that correcting a non-existent match raises an error."""
        match_service = correction_services["match_service"]

        with pytest.raises(ValueError, match="not found"):
            match_service.correct_match_result(
                match_id=99999,
                new_winning_team="dire",
                guild_id=TEST_GUILD_ID,
            )

    def test_double_correction_works(self, correction_services):
        """Test that correcting a match twice (back to original) works."""
        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=7000)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        # Record with Radiant winning
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Correct to Dire
        match_service.correct_match_result(match_id, "dire", TEST_GUILD_ID, corrected_by=1)

        # Correct back to Radiant
        match_service.correct_match_result(match_id, "radiant", TEST_GUILD_ID, corrected_by=1)

        # Verify final state matches original recording
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1
            assert player.losses == 0

        for pid in dire_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 1

        # Should have 2 correction records
        corrections = match_repo.get_match_corrections(match_id)
        assert len(corrections) == 2
