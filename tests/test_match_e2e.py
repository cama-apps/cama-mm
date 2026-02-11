"""
End-to-end tests for match recording functionality.

This consolidated module covers the complete match lifecycle:
- Win/loss recording and accumulation
- Rating updates (Glicko-2)
- State management (shuffle state, double-record prevention)
- Radiant/Dire mapping correctness

Consolidates tests from:
- test_match_recording_basic.py
- test_match_recording_win_loss.py
- test_match_recording_e2e.py
- test_match_recording_ratings.py
- test_match_recording_radiant_dire.py
- test_match_service_win_loss.py
"""

import pytest

from database import Database
from rating_system import CamaRatingSystem
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


# =============================================================================
# FIXTURES
# =============================================================================
# Uses player_repository, match_repository from conftest.py
# Local fixtures only for legacy Database API tests


@pytest.fixture
def test_db(repo_db_path):
    """Create a test database using centralized fast fixture (legacy API)."""
    return Database(repo_db_path)


@pytest.fixture
def match_service_glicko(player_repository, match_repository):
    """Create a MatchService instance with Glicko enabled."""
    return MatchService(player_repo=player_repository, match_repo=match_repository, use_glicko=True)


@pytest.fixture
def test_players_db(test_db):
    """Create 10 test players using Database.add_player (legacy API)."""
    player_ids = list(range(1001, 1011))
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


# =============================================================================
# WIN/LOSS RECORDING TESTS
# =============================================================================


class TestBasicWinLossRecording:
    """Test basic match recording and win/loss tracking."""

    def test_record_match_team1_wins(self, test_db, test_players_db):
        """Test recording a match where team 1 (Radiant) wins."""
        team1_ids = test_players_db[:5]
        team2_ids = test_players_db[5:]

        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        assert match_id is not None

        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1

    def test_record_match_team2_wins(self, test_db, test_players_db):
        """Test recording a match where team 2 (Dire) wins."""
        team1_ids = test_players_db[:5]
        team2_ids = test_players_db[5:]

        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=2)

        assert match_id is not None

        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

    def test_record_multiple_matches_accumulate(self, test_db, test_players_db):
        """Test recording multiple matches and accumulating wins/losses."""
        team1_ids = test_players_db[:5]
        team2_ids = test_players_db[5:]

        # Record 3 matches - team 1 wins first two, team 2 wins third
        test_db.record_match(team1_ids, team2_ids, winning_team=1)
        test_db.record_match(team1_ids, team2_ids, winning_team=1)
        test_db.record_match(team1_ids, team2_ids, winning_team=2)

        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 2
            assert player.losses == 1

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 2


class TestRadiantDireWinLoss:
    """Test win/loss recording using radiant/dire terminology."""

    @pytest.fixture
    def player_ids(self, test_db):
        """Create 10 players in the database."""
        ids = list(range(11001, 11011))
        for pid in ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return ids

    def _fetch_wins_losses(self, test_db, discord_id):
        player = test_db.get_player(discord_id)
        return player.wins, player.losses

    def test_radiant_win_updates_correctly(self, test_db, player_ids):
        """Test that Radiant win updates wins/losses correctly."""
        radiant = player_ids[:5]
        dire = player_ids[5:]

        test_db.record_match(
            radiant_team_ids=radiant,
            dire_team_ids=dire,
            winning_team="radiant",
        )

        for pid in radiant:
            wins, losses = self._fetch_wins_losses(test_db, pid)
            assert wins == 1
            assert losses == 0

        for pid in dire:
            wins, losses = self._fetch_wins_losses(test_db, pid)
            assert wins == 0
            assert losses == 1

    def test_dire_win_updates_correctly(self, test_db, player_ids):
        """Test that Dire win updates wins/losses correctly."""
        radiant = player_ids[:5]
        dire = player_ids[5:]

        test_db.record_match(
            radiant_team_ids=radiant,
            dire_team_ids=dire,
            winning_team="dire",
        )

        for pid in dire:
            wins, losses = self._fetch_wins_losses(test_db, pid)
            assert wins == 1
            assert losses == 0

        for pid in radiant:
            wins, losses = self._fetch_wins_losses(test_db, pid)
            assert wins == 0
            assert losses == 1


# =============================================================================
# MATCH SERVICE INTEGRATION TESTS
# =============================================================================


class TestMatchServiceWinLoss:
    """Test MatchService integration for win/loss recording."""

    def _add_players(self, player_repository, start_id=94001):
        """Helper to add 10 players."""
        ids = list(range(start_id, start_id + 10))
        for idx, pid in enumerate(ids):
            player_repository.add(
                discord_id=pid,
                discord_username=f"MSPlayer{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0 + idx,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return ids

    def _set_last_shuffle(self, service, radiant_ids, dire_ids):
        """Helper to set shuffle state and persist to database."""
        import time
        now_ts = int(time.time())
        state = {
            "radiant_team_ids": radiant_ids,
            "dire_team_ids": dire_ids,
            "excluded_player_ids": [],
            "radiant_roles": ["1", "2", "3", "4", "5"],
            "dire_roles": ["1", "2", "3", "4", "5"],
            "radiant_value": 7500.0,
            "dire_value": 7500.0,
            "value_diff": 0.0,
            "first_pick_team": "Radiant",
            "record_submissions": {},
            "shuffle_timestamp": now_ts,
            "bet_lock_until": now_ts + 900,
            "betting_mode": "pool",
        }
        # Set in-memory and persist to database
        service.set_last_shuffle(TEST_GUILD_ID, state)
        service._persist_match_state(TEST_GUILD_ID, state)

    def test_record_match_updates_wins_and_clears_state(self, player_repository, match_repository):
        """Test that recording a match updates wins/losses and clears state."""
        match_service = MatchService(player_repo=player_repository, match_repo=match_repository, use_glicko=True)

        player_ids = self._add_players(player_repository)
        radiant = player_ids[:5]
        dire = player_ids[5:]

        self._set_last_shuffle(match_service, radiant, dire)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert result["winning_team"] == "radiant"

        for pid in radiant:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1
            assert player.losses == 0

        for pid in dire:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 1

        # State should be cleared after successful record
        assert match_service.get_last_shuffle(TEST_GUILD_ID) is None

    def test_record_match_without_shuffle_fails(self, player_repository, match_repository):
        """Test that recording without a shuffle raises an error."""
        match_service = MatchService(player_repo=player_repository, match_repo=match_repository, use_glicko=True)

        player_ids = self._add_players(player_repository, start_id=95001)
        radiant = player_ids[:5]
        dire = player_ids[5:]

        # No last shuffle set
        with pytest.raises(ValueError):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Ensure no wins/losses were written
        for pid in radiant + dire:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 0

    def test_double_record_prevented(self, player_repository, match_repository):
        """Test that recording twice without reshuffling raises an error."""
        match_service = MatchService(player_repo=player_repository, match_repo=match_repository, use_glicko=True)

        player_ids = self._add_players(player_repository, start_id=96001)
        radiant = player_ids[:5]
        dire = player_ids[5:]

        self._set_last_shuffle(match_service, radiant, dire)
        match_service.record_match("dire", guild_id=TEST_GUILD_ID)

        # Second call without resetting shuffle should fail
        with pytest.raises(ValueError):
            match_service.record_match("dire", guild_id=TEST_GUILD_ID)

        # Verify final state
        for pid in radiant:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 1

        for pid in dire:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1
            assert player.losses == 0


# =============================================================================
# RATING UPDATE TESTS
# =============================================================================


class TestRatingUpdates:
    """Test Glicko-2 rating updates after matches."""

    def test_rating_update_after_match(self, test_db):
        """Test that ratings are updated correctly after a match."""
        rating_system = CamaRatingSystem()

        player1_id = 2001
        player2_id = 2002

        test_db.add_player(
            discord_id=player1_id,
            discord_username="Player1",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        test_db.add_player(
            discord_id=player2_id,
            discord_username="Player2",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Get initial ratings
        initial_rating1, _, _ = test_db.get_player_glicko_rating(player1_id)
        initial_rating2, _, _ = test_db.get_player_glicko_rating(player2_id)

        # Create Glicko-2 players
        player1_glicko = rating_system.create_player_from_rating(initial_rating1, 350.0, 0.06)
        player2_glicko = rating_system.create_player_from_rating(initial_rating2, 350.0, 0.06)

        # Simulate a match where player1 wins
        player1_glicko.update_player([player2_glicko.rating], [player2_glicko.rd], [1.0])
        player2_glicko.update_player([player1_glicko.rating], [player1_glicko.rd], [0.0])

        # Update ratings in database
        test_db.update_player_glicko_rating(
            player1_id, player1_glicko.rating, player1_glicko.rd, player1_glicko.vol
        )
        test_db.update_player_glicko_rating(
            player2_id, player2_glicko.rating, player2_glicko.rd, player2_glicko.vol
        )

        # Check that ratings changed
        new_rating1, _, _ = test_db.get_player_glicko_rating(player1_id)
        new_rating2, _, _ = test_db.get_player_glicko_rating(player2_id)

        # Winner's rating should increase, loser's should decrease
        assert new_rating1 > initial_rating1
        assert new_rating2 < initial_rating2


# =============================================================================
# RADIANT/DIRE BUG FIX VERIFICATION
# =============================================================================


class TestRadiantDireBugFix:
    """
    Tests that verify the Radiant/Dire mapping bug is fixed.

    The original bug: When Dire won, the wrong team got credited with the win.
    These tests ensure the fix works correctly.
    """

    def test_dire_win_credits_correct_team(self, test_db):
        """
        End-to-end test reproducing the exact bug scenario.

        This test simulates:
        1. Shuffle creating teams (Radiant vs Dire)
        2. Recording match with "Dire won"
        3. Verifying leaderboard shows correct wins/losses
        """
        rating_system = CamaRatingSystem()

        # Create players with exact names from bug report
        player_names_and_ratings = [
            # Radiant team
            ("FakeUser917762", 1405),
            ("FakeUser924119", 1120),
            ("FakeUser926408", 1763),
            ("FakeUser921765", 1689),
            ("FakeUser925589", 1568),
            # Dire team
            ("FakeUser923487", 1161),
            ("BugReporter", 1500),
            ("FakeUser921510", 1816),
            ("FakeUser920053", 1500),
            ("FakeUser919197", 1601),
        ]

        player_ids = []
        for idx, (name, rating) in enumerate(player_names_and_ratings):
            discord_id = 93001 + idx
            player_ids.append(discord_id)
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        radiant_team_ids = player_ids[:5]
        dire_team_ids = player_ids[5:]

        # Simulate recording "Dire won" - Dire goes to team1 (winning_team=1)
        match_id = test_db.record_match(
            team1_ids=dire_team_ids,
            team2_ids=radiant_team_ids,
            winning_team=1,  # team1 (Dire) won
        )

        assert match_id is not None

        # Verify all players
        all_players = test_db.get_all_players()

        # Find BugReporter (was on Dire team)
        reporter = next((p for p in all_players if p.name == "BugReporter"), None)
        assert reporter is not None
        assert reporter.wins == 1, f"BugReporter should have 1 win (Dire won), got {reporter.wins}"
        assert reporter.losses == 0, f"BugReporter should have 0 losses, got {reporter.losses}"

        # Verify all Dire players have wins
        dire_player_names = [
            "BugReporter",
            "FakeUser923487",
            "FakeUser921510",
            "FakeUser920053",
            "FakeUser919197",
        ]
        for player in all_players:
            if player.name in dire_player_names:
                assert player.wins == 1, f"Dire player {player.name} should have 1 win"
                assert player.losses == 0, f"Dire player {player.name} should have 0 losses"

        # Verify all Radiant players have losses
        radiant_player_names = [
            "FakeUser917762",
            "FakeUser924119",
            "FakeUser926408",
            "FakeUser921765",
            "FakeUser925589",
        ]
        for player in all_players:
            if player.name in radiant_player_names:
                assert player.wins == 0, f"Radiant player {player.name} should have 0 wins"
                assert player.losses == 1, f"Radiant player {player.name} should have 1 loss"


# =============================================================================
# MATCH PARTICIPANTS TESTS
# =============================================================================


class TestMatchParticipants:
    """Test match participant recording in the database."""

    def test_participants_correctly_recorded(self, test_db, test_players_db, match_repository):
        """Test that match participants are correctly recorded with side and won flags."""
        radiant = test_players_db[:5]
        dire = test_players_db[5:]

        match_id = test_db.record_match(
            radiant_team_ids=radiant,
            dire_team_ids=dire,
            winning_team="radiant",
        )

        # Use repository method to get participants
        participants = match_repository.get_match_participants(match_id)

        assert len(participants) == 10

        radiant_rows = [p for p in participants if p["discord_id"] in radiant]
        dire_rows = [p for p in participants if p["discord_id"] in dire]

        assert len(radiant_rows) == 5
        assert len(dire_rows) == 5

        # Verify Radiant participants
        for row in radiant_rows:
            assert row["team_number"] == 1
            assert row["side"] == "radiant"
            assert row["won"] == 1

        # Verify Dire participants
        for row in dire_rows:
            assert row["team_number"] == 2
            assert row["side"] == "dire"
            assert row["won"] == 0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
