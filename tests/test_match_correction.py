"""
Tests for match correction functionality.

Tests the correct_match_result method that allows admins to fix
incorrectly recorded match results by reversing all effects and
re-applying with the correct winner.
"""

import sqlite3
import time
from types import SimpleNamespace
from unittest import mock

import pytest

from config import JOPACOIN_PER_GAME, JOPACOIN_WIN_REWARD
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match import voting_correction_mixin
from services.match_service import MatchService
from services.vanity_tax_service import VanityTaxService
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


def test_recorded_win_reward_uses_historical_fallback_only_for_legacy_matches():
    assert voting_correction_mixin._recorded_win_reward_jc({"win_reward_jc": 10}) == 10
    assert voting_correction_mixin._recorded_win_reward_jc({"win_reward_jc": None}) == 4


class TestMatchCorrection:
    """Test suite for match result correction."""

    def test_correction_lifecycle_updates_counters_ratings_and_audit(self, correction_services):
        """One record + two corrections cover the whole lifecycle: win/loss
        counters swap, Glicko ratings move the right way when correcting the
        latest match, every correction lands in the audit log, correcting a
        nonexistent match or to the unchanged result is rejected, and a second
        correction back to the original result restores the initial state.

        Merges: test_correction_updates_win_loss_counters,
        test_correction_updates_ratings, test_correction_logs_audit_record,
        test_correction_rejects_same_result,
        test_correction_rejects_nonexistent_match, test_double_correction_works
        and test_correcting_latest_match_still_updates_glicko.
        """
        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        original_ratings = {
            pid: player_repo.get_glicko_rating(pid, TEST_GUILD_ID)[0]
            for pid in player_ids
        }

        # Record with Radiant winning (incorrectly)
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Verify initial state: radiant won (counters + rating direction)
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (1, 0)
            assert player_repo.get_glicko_rating(pid, TEST_GUILD_ID)[0] > original_ratings[pid], \
                "Radiant player should have gained rating from win"
        for pid in dire_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (0, 1)
            assert player_repo.get_glicko_rating(pid, TEST_GUILD_ID)[0] < original_ratings[pid], \
                "Dire player should have lost rating from loss"

        # Guards: nonexistent match and unchanged result are rejected.
        with pytest.raises(ValueError, match="not found"):
            match_service.correct_match_result(
                match_id=99999, new_winning_team="dire", guild_id=TEST_GUILD_ID
            )
        with pytest.raises(ValueError, match="already has radiant as winner"):
            match_service.correct_match_result(
                match_id=match_id, new_winning_team="radiant", guild_id=TEST_GUILD_ID
            )

        # Correct to Dire winning
        admin_id = 88888
        correction_result = match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=admin_id,
        )

        assert correction_result["old_winning_team"] == "radiant"
        assert correction_result["new_winning_team"] == "dire"
        assert correction_result["correction_id"] is not None

        # Verify corrected state: dire won, radiant lost — counters and ratings
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (0, 1), \
                f"Player {pid} should be 0-1 after correction"
            assert player_repo.get_glicko_rating(pid, TEST_GUILD_ID)[0] < original_ratings[pid], \
                "Radiant player should have lost rating after correction"
        for pid in dire_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (1, 0), \
                f"Player {pid} should be 1-0 after correction"
            assert player_repo.get_glicko_rating(pid, TEST_GUILD_ID)[0] > original_ratings[pid], \
                "Dire player should have gained rating after correction"

        # Audit log holds the correction with the admin who made it.
        corrections = match_repo.get_match_corrections(match_id)
        assert len(corrections) == 1
        assert corrections[0]["old_winning_team"] == 1  # Radiant
        assert corrections[0]["new_winning_team"] == 2  # Dire
        assert corrections[0]["corrected_by"] == admin_id

        # Correct back to Radiant: final state matches the original recording.
        match_service.correct_match_result(match_id, "radiant", TEST_GUILD_ID, corrected_by=admin_id)
        for pid in radiant_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (1, 0)
        for pid in dire_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (0, 1)

        # Both corrections are in the audit log.
        assert len(match_repo.get_match_corrections(match_id)) == 2

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
        # the stale payout plus the win bonus the correction reclaims (the
        # radiant bettor was on the old winning team) — no more, no less.
        assert radiant_bet_after["payout"] is None
        assert radiant_balance_after == (
            radiant_balance_after_wrong - radiant_old_payout - JOPACOIN_WIN_REWARD
        ), "Former winner must have exactly its old payout and win bonus reversed"

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

    def test_correction_refunds_old_vanity_tax_and_taxes_new_winner(
        self, correction_services
    ):
        match_service = correction_services["match_service"]
        betting_service = correction_services["betting_service"]
        player_repo = correction_services["player_repo"]
        bet_repo = correction_services["bet_repo"]

        player_ids = _create_players(player_repo, start_id=3200)
        radiant_bettor = 3298
        dire_bettor = 3299
        for bettor in (radiant_bettor, dire_bettor):
            player_repo.add(
                discord_id=bettor,
                discord_username=f"Spectator{bettor}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
            )
            player_repo.add_balance(bettor, TEST_GUILD_ID, 200)

        vanity_tax_service = VanityTaxService()
        vanity_tax_service.refresh_guild(
            TEST_GUILD_ID,
            [
                SimpleNamespace(id=radiant_bettor, nick=None),
                SimpleNamespace(id=dire_bettor, nick=None),
            ],
        )
        betting_service.vanity_tax_service = vanity_tax_service

        match_service.shuffle_players(
            player_ids, guild_id=TEST_GUILD_ID, betting_mode="pool"
        )
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending.bet_lock_until = int(time.time()) + 600
        betting_service.place_bet(
            TEST_GUILD_ID, radiant_bettor, "radiant", 100, pending
        )
        betting_service.place_bet(
            TEST_GUILD_ID, dire_bettor, "dire", 100, pending
        )

        match_service.add_record_submission(
            TEST_GUILD_ID, 99999, "radiant", is_admin=True
        )
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        bets_before = bet_repo.get_settled_bets_for_match(match_id)
        old_payout = next(
            int(bet["payout"])
            for bet in bets_before
            if bet["discord_id"] == radiant_bettor
        )
        old_tax = int((old_payout - 100) * 0.10)
        assert old_tax > 0
        balances_before = {
            bettor: player_repo.get_balance(bettor, TEST_GUILD_ID)
            for bettor in (radiant_bettor, dire_bettor)
        }

        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        bets_after = bet_repo.get_settled_bets_for_match(match_id)
        new_payout = next(
            int(bet["payout"])
            for bet in bets_after
            if bet["discord_id"] == dire_bettor
        )
        new_tax = int((new_payout - 100) * 0.10)
        assert new_tax > 0
        assert player_repo.get_balance(
            radiant_bettor, TEST_GUILD_ID
        ) == balances_before[radiant_bettor] - old_payout + old_tax
        assert player_repo.get_balance(
            dire_bettor, TEST_GUILD_ID
        ) == balances_before[dire_bettor] + new_payout - new_tax
        radiant_history = bet_repo.get_player_bet_history(
            radiant_bettor, TEST_GUILD_ID
        )
        dire_history = bet_repo.get_player_bet_history(
            dire_bettor, TEST_GUILD_ID
        )
        assert sum(row["profit"] for row in radiant_history) == -100
        assert sum(row["profit"] for row in dire_history) == (
            new_payout - 100 - new_tax
        )

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

    def test_correction_moves_win_bonus_exactly(self, correction_services, monkeypatch):
        """Correction must debit the old winners' recorded win bonus and award
        the corrected winners theirs — exact JC per player, and reversible on
        a repeat correction via the win_bonus_jc snapshot. The live config is
        bumped to 99 before correcting, so the exact-10 assertions also prove
        the correction uses the win reward recorded WITH the match, not the
        current config (merges
        test_correction_uses_win_reward_recorded_with_match)."""
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=9000)
        # Capture per-player baselines (players are created with a nonzero
        # starting balance) so every assertion below is an exact delta.
        start = {
            pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in player_ids
        }
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Post-recording baseline: winners hold start + win bonus, losers
        # start + participation (no bets, no streak bonus on day one).
        for pid in radiant_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
                start[pid] + JOPACOIN_WIN_REWARD
            )
        for pid in dire_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
                start[pid] + JOPACOIN_PER_GAME
            )

        # A later balance change must not alter what the correction moves: the
        # recorded win_reward_jc (10) governs, not the live 99.
        monkeypatch.setattr("services.betting_service.JOPACOIN_WIN_REWARD", 99)
        correction = match_service.correct_match_result(
            match_id, "dire", TEST_GUILD_ID, corrected_by=99999
        )
        assert correction["win_bonus_correction"]["reversed"] == dict.fromkeys(radiant_ids, JOPACOIN_WIN_REWARD)
        assert correction["win_bonus_correction"]["awarded"] == dict.fromkeys(dire_ids, JOPACOIN_WIN_REWARD)

        # Old winners: win bonus reclaimed exactly. New winners: keep their
        # participation JC and gain exactly the win bonus.
        for pid in radiant_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == start[pid]
        for pid in dire_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
                start[pid] + JOPACOIN_PER_GAME + JOPACOIN_WIN_REWARD
            )

        # Correcting back reverses the snapshotted award exactly — no drift.
        match_service.correct_match_result(
            match_id, "radiant", TEST_GUILD_ID, corrected_by=99999
        )
        for pid in radiant_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
                start[pid] + JOPACOIN_WIN_REWARD
            )
        for pid in dire_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
                start[pid] + JOPACOIN_PER_GAME
            )

    def test_correction_applies_streak_multipliers(
        self, correction_services, monkeypatch
    ):
        """Corrected ratings must include the streak amplification recording
        the true result would have applied. Seeds a 3-win streak for the true
        winners, corrects, and checks stored ratings against an independent
        recomputation — and that they differ from the no-multiplier values
        the buggy path produced."""
        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=11000)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        # Seed a 3-win streak for every dire player BEFORE the match is
        # recorded, so the corrected result (dire win) continues a 4-game
        # streak while radiant players stay at multiplier 1.0.
        for pid in dire_ids:
            for _ in range(3):
                match_repo.add_rating_history(pid, TEST_GUILD_ID, rating=1500.0, won=True)

        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        bulk_calls = 0
        original_bulk = match_repo.get_player_outcomes_before_match_bulk

        def counted_bulk(*args, **kwargs):
            nonlocal bulk_calls
            bulk_calls += 1
            return original_bulk(*args, **kwargs)

        def fail_point_read(*_args, **_kwargs):
            pytest.fail("correction should use one bulk outcome query")

        monkeypatch.setattr(
            match_repo, "get_player_outcomes_before_match_bulk", counted_bulk
        )
        monkeypatch.setattr(
            match_repo, "get_player_outcomes_before_match", fail_point_read
        )

        match_service.correct_match_result(match_id, "dire", TEST_GUILD_ID, corrected_by=1)

        assert bulk_calls == 1

        # Independent recomputation from the pre-match snapshots.
        history = match_repo.get_full_rating_history_for_match(match_id)
        snap = {e["discord_id"]: e for e in history}
        rating_system = match_service.rating_system

        def _glicko(pid):
            e = snap[pid]
            return rating_system.create_player_from_rating(
                e["rating_before"], e["rd_before"], e["volatility_before"]
            )

        multipliers = {}
        expected_streaks = {}
        for pid in radiant_ids + dire_ids:
            outcomes = [True] * 3 if pid in dire_ids else []
            slen, mult = rating_system.calculate_streak_multiplier(
                outcomes, won=pid in dire_ids
            )
            multipliers[pid] = mult
            expected_streaks[pid] = (slen, mult)
        # The seeded streak must actually amplify — otherwise this test is a
        # tautology that passes even when multipliers are ignored.
        assert all(multipliers[pid] > 1.0 for pid in dire_ids)

        team1, team2 = rating_system.update_ratings_after_match(
            [(_glicko(pid), pid) for pid in dire_ids],
            [(_glicko(pid), pid) for pid in radiant_ids],
            1,
            streak_multipliers=multipliers,
        )
        expected = {pid: rating for rating, _rd, _vol, pid in team1 + team2}
        flat1, flat2 = rating_system.update_ratings_after_match(
            [(_glicko(pid), pid) for pid in dire_ids],
            [(_glicko(pid), pid) for pid in radiant_ids],
            1,
        )
        unamplified = {pid: rating for rating, _rd, _vol, pid in flat1 + flat2}

        for pid in player_ids:
            stored = player_repo.get_glicko_rating(pid, TEST_GUILD_ID)[0]
            assert stored == pytest.approx(expected[pid], abs=1e-6), (
                f"player {pid}: corrected rating must match streak-amplified recompute"
            )
        # The amplified values genuinely differ from the buggy no-multiplier
        # path for the streaking winners.
        assert any(abs(expected[pid] - unamplified[pid]) > 1e-6 for pid in dire_ids)

        # rating_history reflects the corrected streak context.
        conn = sqlite3.connect(correction_services["db_path"])
        conn.row_factory = sqlite3.Row
        rows = conn.execute(
            "SELECT discord_id, streak_length, streak_multiplier FROM rating_history "
            "WHERE match_id = ?",
            (match_id,),
        ).fetchall()
        conn.close()
        stored_streaks = {
            r["discord_id"]: (r["streak_length"], r["streak_multiplier"]) for r in rows
        }
        for pid in player_ids:
            slen, mult = expected_streaks[pid]
            assert stored_streaks[pid] == (slen, pytest.approx(mult))

    def test_correction_preserves_recording_time_streak_rate_after_config_change(
        self, correction_services, monkeypatch
    ):
        """Later config changes must not rewrite an older match's rating curve.

        Records at STREAK_MULTIPLIER_PER_GAME=0.20 (live threshold 3), then
        corrects after the live rate rises to 0.25 AND the live threshold
        rises to 5. The corrected rows must keep the recorded curve: the dire
        4-game streak boosted at the recorded 20% rate (1.40) — the live rate
        would wrongly give 1.50, and the live threshold of 5 would wrongly
        gate it to 1.0. Merges the former separate streak-rate and
        streak-threshold preservation tests."""
        import rating_system as rating_system_module

        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=11500)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        dire_ids = pending.dire_team_ids

        for pid in dire_ids:
            for _ in range(3):
                match_repo.add_rating_history(
                    pid,
                    TEST_GUILD_ID,
                    rating=1500.0,
                    won=True,
                )

        monkeypatch.setattr(
            rating_system_module,
            "STREAK_MULTIPLIER_PER_GAME",
            0.20,
        )
        match_service.add_record_submission(
            TEST_GUILD_ID,
            99999,
            "radiant",
            is_admin=True,
        )
        match_id = match_service.record_match(
            "radiant",
            guild_id=TEST_GUILD_ID,
        )["match_id"]

        # Recording persisted the live threshold (3) per rating_history row.
        conn = sqlite3.connect(correction_services["db_path"])
        thresholds = [
            row[0]
            for row in conn.execute(
                "SELECT streak_threshold FROM rating_history WHERE match_id = ?",
                (match_id,),
            )
        ]
        conn.close()
        assert thresholds and all(value == 3 for value in thresholds)

        # Later balance changes: higher rate AND a gate above the dire streak.
        monkeypatch.setattr(
            rating_system_module,
            "STREAK_MULTIPLIER_PER_GAME",
            0.25,
        )
        monkeypatch.setattr(rating_system_module, "STREAK_THRESHOLD", 5)
        match_service.correct_match_result(
            match_id,
            "dire",
            TEST_GUILD_ID,
            corrected_by=1,
        )

        conn = sqlite3.connect(correction_services["db_path"])
        rows = conn.execute(
            "SELECT discord_id, streak_length, streak_multiplier "
            "FROM rating_history WHERE match_id = ?",
            (match_id,),
        ).fetchall()
        conn.close()
        stored_streaks = {
            discord_id: (streak_length, streak_multiplier)
            for discord_id, streak_length, streak_multiplier in rows
        }

        for pid in dire_ids:
            assert stored_streaks[pid] == (4, pytest.approx(1.40))

    def test_correction_preserves_recording_time_base_delta_multiplier(
        self, correction_services, monkeypatch
    ):
        """A later base-delta change must not rewrite an older match's curve."""
        import rating_system as rating_system_module

        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=11700)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        recorded_multiplier = 0.80
        monkeypatch.setattr(
            rating_system_module,
            "BASE_RATING_DELTA_MULTIPLIER",
            recorded_multiplier,
        )
        match_id = match_service.record_match(
            "radiant",
            guild_id=TEST_GUILD_ID,
        )["match_id"]

        history = match_repo.get_full_rating_history_for_match(match_id)
        snapshots = {entry["discord_id"]: entry for entry in history}
        rating_system = match_service.rating_system

        def _glicko(pid):
            entry = snapshots[pid]
            return rating_system.create_player_from_rating(
                entry["rating_before"],
                entry["rd_before"],
                entry["volatility_before"],
            )

        recorded_team1, recorded_team2 = rating_system.update_ratings_after_match(
            [(_glicko(pid), pid) for pid in dire_ids],
            [(_glicko(pid), pid) for pid in radiant_ids],
            1,
        )
        expected = {
            pid: rating
            for rating, _rd, _vol, pid in recorded_team1 + recorded_team2
        }

        current_multiplier = 0.98
        monkeypatch.setattr(
            rating_system_module,
            "BASE_RATING_DELTA_MULTIPLIER",
            current_multiplier,
        )
        current_team1, current_team2 = rating_system.update_ratings_after_match(
            [(_glicko(pid), pid) for pid in dire_ids],
            [(_glicko(pid), pid) for pid in radiant_ids],
            1,
        )
        wrong_current = {
            pid: rating
            for rating, _rd, _vol, pid in current_team1 + current_team2
        }

        match_service.correct_match_result(
            match_id,
            "dire",
            TEST_GUILD_ID,
            corrected_by=1,
        )

        corrected = {
            entry["discord_id"]: entry
            for entry in match_repo.get_full_rating_history_for_match(match_id)
        }
        for pid in player_ids:
            assert corrected[pid]["base_rating_delta_multiplier"] == pytest.approx(
                recorded_multiplier
            )
            assert corrected[pid]["rating"] == pytest.approx(expected[pid])
            assert corrected[pid]["rating"] != pytest.approx(wrong_current[pid])
            assert player_repo.get_glicko_rating(pid, TEST_GUILD_ID)[0] == pytest.approx(
                expected[pid]
            )

    def test_correction_replay_uses_recorded_streak_rate_for_openskill(
        self, correction_services, monkeypatch
    ):
        """The post-correction replay must retain the match's recorded curve.

        The direct correction update is followed by a full OpenSkill replay, so
        checking only the intermediate update can miss a replay that overwrites
        the streak-amplified value. Real prior matches make the streak available
        to that replay and verify the final player/history values end to end.
        """
        import rating_system as rating_system_module

        match_service = correction_services["match_service"]
        match_repo = correction_services["match_repo"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=11600)
        target = player_ids[0]
        recorded_rate = 0.20
        monkeypatch.setattr(
            rating_system_module,
            "STREAK_MULTIPLIER_PER_GAME",
            recorded_rate,
        )

        # Give the target three replayable wins. Unlike synthetic history rows,
        # these matches are part of the chronological OpenSkill rebuild.
        for _ in range(3):
            match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
            pending = match_service.get_last_shuffle(TEST_GUILD_ID)
            target_side = "radiant" if target in pending.radiant_team_ids else "dire"
            match_service.record_match(target_side, guild_id=TEST_GUILD_ID)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        corrected_side = "radiant" if target in pending.radiant_team_ids else "dire"
        wrong_side = "dire" if corrected_side == "radiant" else "radiant"
        match_id = match_service.record_match(
            wrong_side,
            guild_id=TEST_GUILD_ID,
        )["match_id"]

        # A later balance change must not affect correction or its replay.
        current_rate = 0.25
        monkeypatch.setattr(
            rating_system_module,
            "STREAK_MULTIPLIER_PER_GAME",
            current_rate,
        )
        match_service.correct_match_result(
            match_id,
            corrected_side,
            TEST_GUILD_ID,
            corrected_by=1,
        )

        history = match_repo.get_full_rating_history_for_match(match_id)
        snapshots = {entry["discord_id"]: entry for entry in history}
        target_snapshot = snapshots[target]
        radiant_ids = pending.radiant_team_ids
        dire_ids = pending.dire_team_ids

        native = match_service.openskill_system.update_ratings_equal_weight(
            [
                (
                    pid,
                    snapshots[pid]["os_mu_before"],
                    snapshots[pid]["os_sigma_before"],
                )
                for pid in radiant_ids
            ],
            [
                (
                    pid,
                    snapshots[pid]["os_mu_before"],
                    snapshots[pid]["os_sigma_before"],
                )
                for pid in dire_ids
            ],
            winning_team=1 if corrected_side == "radiant" else 2,
        )
        old_mu = target_snapshot["os_mu_before"]
        native_delta = native[target][0] - old_mu
        recorded_multiplier = 1.0 + recorded_rate * 2  # fourth consecutive win
        current_multiplier = 1.0 + current_rate * 2
        expected_mu = old_mu + native_delta * recorded_multiplier
        wrong_current_mu = old_mu + native_delta * current_multiplier

        assert target_snapshot["streak_multiplier_per_game"] == pytest.approx(recorded_rate)
        assert target_snapshot["os_mu_after"] == pytest.approx(expected_mu)
        assert target_snapshot["os_mu_after"] != pytest.approx(native[target][0])
        assert target_snapshot["os_mu_after"] != pytest.approx(wrong_current_mu)
        assert player_repo.get_openskill_rating(target, TEST_GUILD_ID)[0] == pytest.approx(
            expected_mu
        )

        with match_repo.connection() as conn:
            streak = conn.execute(
                "SELECT streak_length, streak_multiplier "
                "FROM rating_history WHERE match_id = ? AND discord_id = ?",
                (match_id, target),
            ).fetchone()
        assert (streak["streak_length"], streak["streak_multiplier"]) == (
            4,
            pytest.approx(recorded_multiplier),
        )


def test_correction_finishes_after_partial_win_bonus_failure(correction_services):
    """A failure while awarding corrected winners must abort BEFORE the
    winning_team flip so the identical correction stays retryable, and the
    retry must complete the payout exactly once per player — skipping the
    winner already paid on the failed attempt."""
    match_service = correction_services["match_service"]
    betting_service = correction_services["betting_service"]
    player_repo = correction_services["player_repo"]
    match_repo = correction_services["match_repo"]

    player_ids = _create_players(player_repo, start_id=14000)
    start = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in player_ids}
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    radiant_ids = pending.radiant_team_ids
    dire_ids = pending.dire_team_ids
    match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
    match_id = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)["match_id"]

    # Fail the award for the SECOND corrected winner: the first is already
    # paid and snapshotted when the correction dies.
    real_award = betting_service.award_win_bonus
    calls = {"n": 0}

    def flaky_award(*args, **kwargs):
        calls["n"] += 1
        if calls["n"] == 2:
            raise sqlite3.OperationalError("database is locked")
        return real_award(*args, **kwargs)

    betting_service.award_win_bonus = flaky_award
    try:
        with pytest.raises(sqlite3.OperationalError):
            match_service.correct_match_result(
                match_id, "dire", TEST_GUILD_ID, corrected_by=99999
            )
    finally:
        betting_service.award_win_bonus = real_award

    # The flip never happened, so the same correction can simply be re-run.
    assert match_repo.get_match(match_id, TEST_GUILD_ID)["winning_team"] == 1

    match_service.correct_match_result(match_id, "dire", TEST_GUILD_ID, corrected_by=99999)

    # Exactly-once for every player: old winners fully reversed (once), new
    # winners hold participation plus exactly one win bonus.
    for pid in radiant_ids:
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == start[pid]
    for pid in dire_ids:
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
            start[pid] + JOPACOIN_PER_GAME + JOPACOIN_WIN_REWARD
        )


def test_win_bonus_reversal_marks_rows_and_never_double_debits(correction_services):
    """The reversal must mark reversed rows with win_bonus_jc = 0 — not NULL,
    because NULL means 'legacy pre-snapshot match' and falls back to a gross
    debit — so recomputing the debits the way the correction does yields
    nothing and a repeat reversal cannot double-debit."""
    match_service = correction_services["match_service"]
    match_repo = correction_services["match_repo"]
    player_repo = correction_services["player_repo"]

    player_ids = _create_players(player_repo, start_id=15000)
    match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
    pending = match_service.get_last_shuffle(TEST_GUILD_ID)
    radiant_ids = pending.radiant_team_ids
    match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
    match_id = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)["match_id"]

    def compute_debits():
        participants = {
            p["discord_id"]: p
            for p in match_repo.get_match_participants(match_id, TEST_GUILD_ID)
        }
        debits = {}
        for pid in radiant_ids:
            stored = participants.get(pid, {}).get("win_bonus_jc")
            amount = int(stored) if stored is not None else JOPACOIN_WIN_REWARD
            if amount > 0:
                debits[pid] = amount
        return debits

    first = compute_debits()
    assert first == dict.fromkeys(radiant_ids, JOPACOIN_WIN_REWARD)
    before = {pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in radiant_ids}
    match_repo.apply_win_bonus_reversal_atomic(match_id, TEST_GUILD_ID, first)

    participants = {
        p["discord_id"]: p
        for p in match_repo.get_match_participants(match_id, TEST_GUILD_ID)
    }
    for pid in radiant_ids:
        assert participants[pid]["win_bonus_jc"] == 0
    assert compute_debits() == {}
    for pid in radiant_ids:
        assert (
            player_repo.get_balance(pid, TEST_GUILD_ID)
            == before[pid] - JOPACOIN_WIN_REWARD
        )


class TestCorrectionDoesNotRewindLaterMatches:
    """Correcting an older match must not clobber ratings earned since."""

    def test_correcting_older_match_preserves_current_glicko(self, correction_services):
        """A second match's rating gains survive a correction of the first match.

        The correction recomputes Glicko from the corrected match's
        ``rating_before``. Writing that back for a player who has played since
        would rewind their live rating to match #1's post-values and discard
        match #2 entirely — and unlike OpenSkill there is no Glicko replay to
        repair it.
        """
        match_service = correction_services["match_service"]
        player_repo = correction_services["player_repo"]

        player_ids = _create_players(player_repo, start_id=7000)

        # Match 1 — recorded with radiant winning (the result we later correct).
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending1 = match_service.get_last_shuffle(TEST_GUILD_ID)
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        first = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        first_match_id = first["match_id"]

        # Match 2 — played afterwards by the same roster.
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        match_service.get_last_shuffle(TEST_GUILD_ID)
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        ratings_after_two_matches = {
            pid: player_repo.get_by_id(pid, TEST_GUILD_ID).glicko_rating
            for pid in player_ids
        }

        # Correct match 1 — the stale one.
        match_service.correct_match_result(
            match_id=first_match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        for pid in player_ids:
            assert player_repo.get_by_id(pid, TEST_GUILD_ID).glicko_rating == pytest.approx(
                ratings_after_two_matches[pid]
            ), (
                f"Player {pid}'s live rating was rewound by correcting an older "
                "match, discarding match 2's rating change"
            )

        # The corrected match's own history row must still reflect the new result.
        history = correction_services["match_repo"].get_rating_history_for_match(first_match_id)
        by_id = {row["discord_id"]: row for row in history}
        for pid in pending1.dire_team_ids:
            assert by_id[pid]["won"] == 1, "corrected history must show dire winning"

class TestWinBonusIsNotPaidTwiceOnRetry:
    """A correction that dies between the credit and its snapshot must not repay.

    award_win_bonus credits the balance, then update_participant_bonus_jc
    records how much — in a separate transaction. A crash in between left the
    player looking unpaid, so re-running the correction paid them again. The
    economy ledger row is written by the balance trigger inside the credit's
    own transaction, so it is the marker that cannot disagree with the money.
    """

    def test_ledger_marks_the_bonus_as_paid_within_the_credit_transaction(
        self, correction_services,
    ):
        betting_service = correction_services["betting_service"]
        player_repo = correction_services["player_repo"]
        match_repo = correction_services["match_repo"]

        pid = 8100
        _create_players(player_repo, start_id=pid, count=1)

        assert match_repo.get_win_bonus_credited_ids(4242, TEST_GUILD_ID) == set()

        betting_service.award_win_bonus([pid], TEST_GUILD_ID, match_id=4242)

        assert match_repo.get_win_bonus_credited_ids(4242, TEST_GUILD_ID) == {pid}, (
            "the credit left no atomic record that it happened"
        )
        # Scoped to the match it was paid for.
        assert match_repo.get_win_bonus_credited_ids(4243, TEST_GUILD_ID) == set()

    def test_correction_retry_does_not_repay_a_credited_winner(
        self, correction_services,
    ):
        """Simulate the crash: credit lands, snapshot never does."""
        match_service = correction_services["match_service"]
        betting_service = correction_services["betting_service"]
        player_repo = correction_services["player_repo"]
        match_repo = correction_services["match_repo"]

        player_ids = _create_players(player_repo, start_id=8200)
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        match_service.add_record_submission(TEST_GUILD_ID, 99999, "radiant", is_admin=True)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # A previous correction attempt paid this dire player, then died before
        # writing the win_bonus_jc snapshot.
        victim = pending.dire_team_ids[0]
        betting_service.award_win_bonus([victim], TEST_GUILD_ID, match_id=match_id)
        balance_after_first_payment = player_repo.get_balance(victim, TEST_GUILD_ID)
        # The original recording already credited the radiant winners for this
        # match, so the set holds them too; what matters is that our victim is
        # now recorded as paid.
        assert victim in match_repo.get_win_bonus_credited_ids(match_id, TEST_GUILD_ID)

        # The admin re-runs the correction. The snapshot is still NULL.
        match_service.correct_match_result(
            match_id=match_id,
            new_winning_team="dire",
            guild_id=TEST_GUILD_ID,
            corrected_by=99999,
        )

        assert player_repo.get_balance(victim, TEST_GUILD_ID) == balance_after_first_payment, (
            "the win bonus was paid a second time on the correction retry"
        )
