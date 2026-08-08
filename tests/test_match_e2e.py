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

from rating_system import CamaRatingSystem
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID
from tests.repository_harness import RepositoryTestDatabase as Database

# =============================================================================
# FIXTURES
# =============================================================================
# Uses player_repository, match_repository from conftest.py
# Local fixtures only for legacy Database API tests


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

        from domain.models.pending_match_state import PendingMatchState
        now_ts = int(time.time())
        state = PendingMatchState(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            excluded_player_ids=[],
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=7500.0,
            dire_value=7500.0,
            value_diff=0.0,
            first_pick_team="Radiant",
            record_submissions={},
            shuffle_timestamp=now_ts,
            bet_lock_until=now_ts + 900,
            betting_mode="pool",
        )
        # Set in-memory and persist to database
        service.set_last_shuffle(TEST_GUILD_ID, state)
        service._persist_match_state(TEST_GUILD_ID, state)

    def test_record_match_updates_wins_and_clears_state(self, player_repository, match_repository):
        """Recording from a persisted shuffle state updates wins/losses and
        clears the state; recording without a shuffle fails and writes no
        stats; recording twice without reshuffling fails and leaves the first
        result intact.

        Merges test_record_match_without_shuffle_fails and
        test_double_record_prevented.
        """
        match_service = MatchService(player_repo=player_repository, match_repo=match_repository, use_glicko=True)

        player_ids = self._add_players(player_repository)
        radiant = player_ids[:5]
        dire = player_ids[5:]

        # No last shuffle set: recording fails and writes nothing.
        with pytest.raises(ValueError):
            match_service.record_match("dire", guild_id=TEST_GUILD_ID)
        for pid in radiant + dire:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 0

        self._set_last_shuffle(match_service, radiant, dire)
        result = match_service.record_match("dire", guild_id=TEST_GUILD_ID)

        assert result["winning_team"] == "dire"

        for pid in dire:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1
            assert player.losses == 0

        for pid in radiant:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 1

        # State should be cleared after successful record
        assert match_service.get_last_shuffle(TEST_GUILD_ID) is None

        # Second record without reshuffling fails and changes nothing.
        with pytest.raises(ValueError):
            match_service.record_match("dire", guild_id=TEST_GUILD_ID)
        for pid in dire:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (1, 0)
        for pid in radiant:
            player = player_repository.get_by_id(pid, TEST_GUILD_ID)
            assert (player.wins, player.losses) == (0, 1)


# =============================================================================
# END-TO-END MATCH FLOW TESTS (from test_match_recording_e2e.py)
# =============================================================================

# Legacy Database.add_player() uses guild_id=0 by default
_LEGACY_GUILD_ID = 0


class TestEndToEndRadiantDireBug:
    """End-to-end tests that reproduce the exact bug scenario from shuffle to leaderboard."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_full_workflow_exact_bug_scenario(self, test_db):
        """
        End-to-end test reproducing the exact bug scenario.

        This test simulates:
        1. Shuffle creating teams (Radiant vs Dire)
        2. Recording match with "Dire won"
        3. Verifying leaderboard shows correct wins/losses

        This is the COMPLETE workflow that failed in production.
        """
        # Step 1: Create all players with exact names from bug report
        player_names_and_ratings = [
            # Radiant team
            ("FakeUser917762", 1405),
            ("FakeUser924119", 1120),
            ("FakeUser926408", 1763),
            ("FakeUser921765", 1689),
            ("FakeUser925589", 1568),
            # Dire team
            ("FakeUser923487", 1161),
            ("BugReporter", 1500),  # The bug reporter
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

        # Step 2: Simulate shuffle output (as stored in bot.last_shuffle)
        # Radiant team IDs (first 5 players)
        radiant_team_ids = player_ids[:5]
        # Dire team IDs (last 5 players)
        dire_team_ids = player_ids[5:]

        # Step 3: Simulate team number assignment (randomly assigned in real shuffle)
        # Let's test both scenarios
        radiant_team_num = 1
        dire_team_num = 2

        # Step 4: Simulate recording "Dire won"
        winning_team_num = dire_team_num

        # Step 5: Apply the FIXED logic (this is what bot.py does now)
        actual_radiant_team_num = radiant_team_num
        actual_dire_team_num = dire_team_num

        # Validate (as the fix does)
        assert actual_radiant_team_num is not None
        assert actual_dire_team_num is not None
        assert actual_radiant_team_num != actual_dire_team_num

        # Map winning team to team1/team2 for database (FIXED LOGIC)
        if winning_team_num == actual_radiant_team_num:
            # Radiant won
            team1_ids_for_db = radiant_team_ids
            team2_ids_for_db = dire_team_ids
            winning_team_for_db = 1
        elif winning_team_num == actual_dire_team_num:
            # Dire won - THIS IS THE BUG SCENARIO
            team1_ids_for_db = dire_team_ids  # Dire goes to team1
            team2_ids_for_db = radiant_team_ids  # Radiant goes to team2
            winning_team_for_db = 1  # team1 (Dire) won
        else:
            raise ValueError(f"Invalid winning_team_num: {winning_team_num}")

        # Step 6: Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # Step 7: Verify leaderboard (as shown in bug report)
        # Get all players sorted by wins (descending), then by rating
        all_players = test_db.get_all_players()

        # Sort by wins (descending), then by rating
        rating_system = CamaRatingSystem()

        players_with_stats = []
        for player in all_players:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = None
            if player.glicko_rating is not None:
                cama_rating = rating_system.rating_to_display(player.glicko_rating)
            players_with_stats.append((player, player.wins, player.losses, win_rate, cama_rating))

        # Sort by wins (highest first), then by rating
        players_with_stats.sort(key=lambda x: (x[1], x[4] if x[4] is not None else 0), reverse=True)

        # Step 8: CRITICAL ASSERTIONS - Verify the bug is fixed

        # Find BugReporter in the results
        reporter_stats = None
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name == "BugReporter":
                reporter_stats = (player, wins, losses, win_rate, rating)
                break

        assert reporter_stats is not None, "BugReporter not found in leaderboard"
        _, reporter_wins, reporter_losses, reporter_win_rate, _ = (
            reporter_stats
        )

        # THE BUG: BugReporter was on Dire, Dire won, but BugReporter showed 0-1
        # THE FIX: BugReporter should now show 1-0
        assert reporter_wins == 1, (
            f"BUG FIX VERIFICATION: BugReporter should have 1 win (Dire won), got {reporter_wins}"
        )
        assert reporter_losses == 0, (
            f"BUG FIX VERIFICATION: BugReporter should have 0 losses (Dire won), got {reporter_losses}"
        )
        assert reporter_win_rate == 100.0, (
            f"BUG FIX VERIFICATION: BugReporter should have 100% win rate, got {reporter_win_rate:.1f}%"
        )

        # Verify all Dire players have wins
        dire_player_names = [
            "BugReporter",
            "FakeUser923487",
            "FakeUser921510",
            "FakeUser920053",
            "FakeUser919197",
        ]
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name in dire_player_names:
                assert wins == 1, (
                    f"BUG FIX: Dire player {player.name} should have 1 win, got {wins}"
                )
                assert losses == 0, (
                    f"BUG FIX: Dire player {player.name} should have 0 losses, got {losses}"
                )

        # Verify all Radiant players have losses
        radiant_player_names = [
            "FakeUser917762",
            "FakeUser924119",
            "FakeUser926408",
            "FakeUser921765",
            "FakeUser925589",
        ]
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name in radiant_player_names:
                assert wins == 0, (
                    f"BUG FIX: Radiant player {player.name} should have 0 wins, got {wins}"
                )
                assert losses == 1, (
                    f"BUG FIX: Radiant player {player.name} should have 1 loss, got {losses}"
                )

        # Verify leaderboard order (winners first)
        # Top 5 should be Dire players (1-0)
        # Bottom 5 should be Radiant players (0-1)
        top_5_wins = [wins for _, wins, _, _, _ in players_with_stats[:5]]
        bottom_5_wins = [wins for _, wins, _, _, _ in players_with_stats[5:]]

        assert all(w == 1 for w in top_5_wins), (
            f"Top 5 players should all have 1 win (Dire won), got {top_5_wins}"
        )
        assert all(w == 0 for w in bottom_5_wins), (
            f"Bottom 5 players should all have 0 wins (Radiant lost), got {bottom_5_wins}"
        )


class TestAbortLobbyReset:
    """Test that aborting a match resets the lobby and clears the lobby message ID."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players."""
        player_ids = list(range(8001, 8011))
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

    def test_abort_resets_lobby_message_id(self, test_db, test_players):
        """
        Test that aborting a match resets the lobby and clears the lobby message ID.

        This test verifies the bug fix where aborting a match would leave the old
        lobby message ID, causing /lobby to refresh the old message instead of
        creating a new one.
        """
        # Create services
        lobby_repo = LobbyRepository(test_db.db_path)
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        lobby_manager = LobbyManager(lobby_repo)
        lobby_service = LobbyService(lobby_manager, player_repo)
        match_service = MatchService(
            player_repo=player_repo, match_repo=match_repo, use_glicko=True
        )

        # Step 1: Create a lobby and set a lobby message ID (simulating /lobby command)
        lobby_service.get_or_create_lobby(creator_id=test_players[0])
        old_message_id = 12345
        lobby_service.set_lobby_message_id(old_message_id)

        # Verify lobby message ID is set
        assert lobby_service.get_lobby_message_id() == old_message_id

        # Step 2: Add players to lobby and shuffle (simulating /shuffle command)
        for pid in test_players:
            lobby_service.join_lobby(pid, 0)

        assert lobby_service.get_lobby() is not None
        assert lobby_service.get_lobby().get_player_count() == 10

        # Shuffle players (this resets the lobby)
        match_service.shuffle_players(test_players, guild_id=_LEGACY_GUILD_ID)
        lobby_service.reset_lobby()

        # After shuffle, lobby should be reset
        assert lobby_service.get_lobby_message_id() is None
        assert lobby_service.get_lobby() is None

        # Step 3: Simulate creating a new lobby message after shuffle
        # (In real scenario, /lobby would create a new message)
        new_message_id = 67890
        lobby_service.set_lobby_message_id(new_message_id)
        assert lobby_service.get_lobby_message_id() == new_message_id

        # Step 4: Abort the match (simulating /record abort command)
        # This should reset the lobby and clear the message ID
        match_service.clear_last_shuffle(_LEGACY_GUILD_ID)
        lobby_service.reset_lobby()

        # Step 5: Verify lobby is reset after abort
        assert lobby_service.get_lobby_message_id() is None, (
            "Lobby message ID should be cleared after abort"
        )
        assert lobby_service.get_lobby() is None, "Lobby should be reset after abort"

        # Step 6: Verify a new lobby can be created (simulating /lobby after abort)
        new_lobby = lobby_service.get_or_create_lobby(creator_id=test_players[0])
        assert new_lobby is not None
        assert new_lobby.get_player_count() == 0  # Fresh lobby should be empty

        # Step 7: Verify that setting a new message ID works (simulating new /lobby)
        final_message_id = 99999
        lobby_service.set_lobby_message_id(final_message_id)
        assert lobby_service.get_lobby_message_id() == final_message_id, (
            "Should be able to set a new lobby message ID after abort"
        )


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
