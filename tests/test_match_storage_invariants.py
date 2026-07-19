"""
Tests for the new match storage invariants:
- team1_players = Radiant, team2_players = Dire
- winning_team = 1 (Radiant won) or 2 (Dire won)
- match_participants.side is populated for all participants
"""

import pytest

from config import JOPACOIN_PER_GAME, JOPACOIN_WIN_REWARD
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.repository_harness import RepositoryTestDatabase as Database

TEST_GUILD_ID = 123


class TestMatchStorageInvariants:
    """Test the Radiant/Dire storage invariants."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players in the database."""
        player_ids = list(range(7001, 7011))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return player_ids

    def test_radiant_wins_stores_winning_team_1(self, test_db, test_players):
        """Test that Radiant winning stores winning_team=1."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        match_id = test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="radiant",
        )

        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT team1_players, team2_players, winning_team FROM matches WHERE match_id = ?",
            (match_id,),
        )
        row = cursor.fetchone()
        conn.close()

        import json

        assert json.loads(row["team1_players"]) == radiant_ids, "team1 should be Radiant"
        assert json.loads(row["team2_players"]) == dire_ids, "team2 should be Dire"
        assert row["winning_team"] == 1, "Radiant winning should store winning_team=1"

    def test_dire_wins_stores_winning_team_2(self, test_db, test_players):
        """Test that Dire winning stores winning_team=2."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        match_id = test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="dire",
        )

        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT team1_players, team2_players, winning_team FROM matches WHERE match_id = ?",
            (match_id,),
        )
        row = cursor.fetchone()
        conn.close()

        import json

        assert json.loads(row["team1_players"]) == radiant_ids, "team1 should be Radiant"
        assert json.loads(row["team2_players"]) == dire_ids, "team2 should be Dire"
        assert row["winning_team"] == 2, "Dire winning should store winning_team=2"

    def test_match_participants_side_populated(self, test_db, test_players):
        """Test that match_participants.side is populated for all participants."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        match_id = test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="radiant",
        )

        conn = test_db.get_connection()
        cursor = conn.cursor()

        # Check Radiant players have side='radiant'
        for pid in radiant_ids:
            cursor.execute(
                "SELECT side FROM match_participants WHERE match_id = ? AND discord_id = ?",
                (match_id, pid),
            )
            row = cursor.fetchone()
            assert row is not None, f"Player {pid} should be in match_participants"
            assert row["side"] == "radiant", f"Player {pid} should have side='radiant'"

        # Check Dire players have side='dire'
        for pid in dire_ids:
            cursor.execute(
                "SELECT side FROM match_participants WHERE match_id = ? AND discord_id = ?",
                (match_id, pid),
            )
            row = cursor.fetchone()
            assert row is not None, f"Player {pid} should be in match_participants"
            assert row["side"] == "dire", f"Player {pid} should have side='dire'"

        conn.close()

    def test_wins_losses_correct_for_radiant_win(self, test_db, test_players):
        """Test that wins/losses are correct when Radiant wins."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="radiant",
        )

        # Radiant players should have wins
        for pid in radiant_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Radiant player {pid} should have 1 win"
            assert player.losses == 0, f"Radiant player {pid} should have 0 losses"

        # Dire players should have losses
        for pid in dire_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Dire player {pid} should have 0 wins"
            assert player.losses == 1, f"Dire player {pid} should have 1 loss"

    def test_wins_losses_correct_for_dire_win(self, test_db, test_players):
        """Test that wins/losses are correct when Dire wins."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="dire",
        )

        # Dire players should have wins
        for pid in dire_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Dire player {pid} should have 1 win"
            assert player.losses == 0, f"Dire player {pid} should have 0 losses"

        # Radiant players should have losses
        for pid in radiant_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Radiant player {pid} should have 0 wins"
            assert player.losses == 1, f"Radiant player {pid} should have 1 loss"


class TestConcurrencyGuard:
    """Test the concurrency guard for match recording."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def player_repo(self, test_db):
        """Create a PlayerRepository instance."""
        return PlayerRepository(test_db.db_path)

    @pytest.fixture
    def test_players(self, test_db, player_repo):
        """Create 10 test players in the database."""
        player_ids = list(range(8001, 8011))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
        return player_ids

    def test_double_record_fails(self, test_db, player_repo, test_players):
        """Test that attempting to record twice fails."""
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(
            player_repo=player_repo, match_repo=match_repo, use_glicko=True
        )

        # Shuffle to create pending match
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)

        # First record should succeed
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        assert result["match_id"] is not None

        # Second record should fail (no pending match)
        with pytest.raises(ValueError, match="No recent shuffle found"):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    def test_no_double_record_when_post_core_step_fails(
        self, test_db, player_repo, test_players
    ):
        """Regression: a post-core failure must not let a retry double-record
        — nor double-pay the post-core bonus credits.

        record_match commits the match (matches row + win/loss + rating writes)
        in record_match_core_atomic, then runs the money side (bet settlement,
        loan repayment) AFTER that txn. Previously the pending shuffle was only
        cleared after those unguarded post-core steps, so if one raised, the
        pending row survived and a user retry re-ran the atomic core — producing
        a DUPLICATE matches row plus a second win/loss for every player.

        The fix keys the matches row on the pending match: a retry re-enters
        record_match, the idempotency guard returns the already-recorded match
        (no duplicate, no second win/loss), and the post-core money steps re-run
        to settle what the failure stranded — the pending row is kept until those
        steps succeed. Exactly one match exists with wins/losses applied once.

        The bonus credits (participation, win bonus, streak bonus) are gated by
        the per-match bonuses_paid claim: the failing first attempt pays them,
        the retry fails to claim and skips them — balances credited exactly once.
        """
        match_repo = MatchRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=True,
            betting_service=betting_service,
        )

        # Capture per-player baselines (players are created with a nonzero
        # starting balance) so the exactly-once assertions are exact deltas.
        start = {
            pid: player_repo.get_balance(pid, TEST_GUILD_ID) for pid in test_players
        }

        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        radiant_ids = list(pending.radiant_team_ids)
        dire_ids = list(pending.dire_team_ids)

        # Simulate a post-core step raising AFTER the atomic core commits. The
        # atomic match/win-loss/rating writes have already landed at this point.
        original_repay = match_service._repay_outstanding_loans

        def _boom(*args, **kwargs):
            raise RuntimeError("simulated post-core failure (loan repayment)")

        match_service._repay_outstanding_loans = _boom
        with pytest.raises(RuntimeError, match="simulated post-core failure"):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Restore the real post-core step and retry, exactly as a user would
        # after the bot told them to "try again". The pending shuffle still
        # exists (it is cleared only once the post-core steps succeed), so the
        # retry re-enters record_match, the idempotency guard returns the
        # already-recorded match (no duplicate, no second win/loss), and the
        # post-core money steps re-run to recover — instead of stranding.
        match_service._repay_outstanding_loans = original_repay
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Exactly one match recorded — not two.
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT COUNT(*) AS c FROM matches WHERE guild_id = ?", (TEST_GUILD_ID,)
        )
        match_count = cursor.fetchone()["c"]
        conn.close()
        assert match_count == 1, "post-core failure + retry must not double-record"

        # Win/loss applied exactly once per player (5 wins on radiant, 5 losses).
        players = [player_repo.get_by_id(pid, TEST_GUILD_ID) for pid in test_players]
        for pid, player in zip(test_players, players, strict=True):
            assert player.wins + player.losses == 1, (
                f"player {pid} should have exactly one game, "
                f"got {player.wins}W/{player.losses}L"
            )
        assert sum(p.wins for p in players) == 5
        assert sum(p.losses for p in players) == 5

        # Bonuses credited exactly once: the failing first attempt paid them
        # (the injected failure hit AFTER the bonus step), and the retry's
        # claim on matches.bonuses_paid fails, so it must not re-pay. Winners
        # hold exactly one win bonus, losers exactly one participation credit
        # (no bets placed, no streak bonus on a day-one streak).
        for pid in radiant_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
                start[pid] + JOPACOIN_WIN_REWARD
            ), f"winner {pid} must be credited the win bonus exactly once"
        for pid in dire_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == (
                start[pid] + JOPACOIN_PER_GAME
            ), f"loser {pid} must be credited participation exactly once"

    def test_consume_pending_match_is_single_use_idempotent(self, test_db):
        """consume_pending_match is single-use: the first call returns the exact
        saved payload (plus the pending_match_id), and any later call for the
        same guild returns None because the row was deleted in the same txn.

        This pins sequential idempotency (the consuming DELETE is committed
        before the payload is returned). It does NOT exercise true concurrency;
        the BEGIN IMMEDIATE write-lock that makes two simultaneous consumers
        safe is covered by the record_match double-record guards above.
        """
        # Save a pending match with a distinctive multi-field payload so the
        # round-trip can't accidentally pass on a partial/empty return.
        payload = {"test": "data", "team": [1, 2, 3], "winning_team": 2}
        pending_match_id = test_db.save_pending_match(123, payload)

        # First consume returns EXACTLY the saved payload plus the id key —
        # nothing dropped, nothing extra beyond pending_match_id.
        result1 = test_db.consume_pending_match(123)
        assert result1 == {**payload, "pending_match_id": pending_match_id}

        # Row is gone: a second consume yields None (the DELETE committed).
        assert test_db.consume_pending_match(123) is None
        # And a third consume is still None — the None is sticky, not flapping.
        assert test_db.consume_pending_match(123) is None


class TestOldApiCompatibility:
    """Test backward compatibility with old team1/team2 API."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players in the database."""
        player_ids = list(range(9001, 9011))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return player_ids

    def test_old_api_team1_wins(self, test_db, test_players):
        """Test old API with team1 winning."""
        team1_ids = test_players[:5]
        team2_ids = test_players[5:]

        test_db.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=1,
        )

        # Team1 (treated as Radiant) should have wins
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Team1 player {pid} should have 1 win"
            assert player.losses == 0, f"Team1 player {pid} should have 0 losses"

        # Team2 (treated as Dire) should have losses
        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Team2 player {pid} should have 0 wins"
            assert player.losses == 1, f"Team2 player {pid} should have 1 loss"

    def test_old_api_team2_wins(self, test_db, test_players):
        """Test old API with team2 winning."""
        team1_ids = test_players[:5]
        team2_ids = test_players[5:]

        test_db.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=2,
        )

        # Team1 (treated as Radiant) should have losses
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Team1 player {pid} should have 0 wins"
            assert player.losses == 1, f"Team1 player {pid} should have 1 loss"

        # Team2 (treated as Dire) should have wins
        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Team2 player {pid} should have 1 win"
            assert player.losses == 0, f"Team2 player {pid} should have 0 losses"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
