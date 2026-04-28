"""
Consolidated tests for match recording.

Merges the previously fragmented test_match_recording_* files into one module:
basic win/loss recording, radiant/dire mapping, betting integration, admin
override + voting, logging, and bug regressions.
"""

import logging

import pytest

from config import JOPACOIN_EXCLUSION_REWARD, JOPACOIN_WIN_REWARD
from database import Database
from domain.models.team import Team
from rating_system import CamaRatingSystem
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID

# =============================================================================
# SHARED FIXTURES
# =============================================================================


@pytest.fixture
def match_recording_players(test_db_with_schema):
    """Seed 10 players (1001-1010) directly via Database.add_player.

    Canonical setup for tests that exercise the legacy ``record_match`` API on
    the Database object. Returns the list of discord_ids.
    """
    player_ids = [1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010]
    for pid in player_ids:
        test_db_with_schema.add_player(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return player_ids


def _seed_repo_players(player_repo, player_ids):
    """Helper: seed players via PlayerRepository (used by service-layer tests)."""
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return player_ids


# =============================================================================
# === Win/Loss API ===
# =============================================================================


class TestMatchRecordingBasic:
    """Test match recording and win/loss tracking (legacy team1/team2 API)."""

    def test_record_match_team1_wins(self, test_db_with_schema, match_recording_players):
        """Test recording a match where team 1 wins."""
        team1_ids = match_recording_players[:5]  # First 5 players
        team2_ids = match_recording_players[5:]  # Last 5 players

        # Record match - team 1 wins
        match_id = test_db_with_schema.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        assert match_id is not None

        # Check win/loss counts
        for pid in team1_ids:
            player = test_db_with_schema.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

        for pid in team2_ids:
            player = test_db_with_schema.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1

    def test_record_match_team2_wins(self, test_db_with_schema, match_recording_players):
        """Test recording a match where team 2 wins."""
        team1_ids = match_recording_players[:5]
        team2_ids = match_recording_players[5:]

        # Record match - team 2 wins
        match_id = test_db_with_schema.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=2)

        assert match_id is not None

        # Check win/loss counts
        for pid in team1_ids:
            player = test_db_with_schema.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1

        for pid in team2_ids:
            player = test_db_with_schema.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

    def test_record_multiple_matches(self, test_db_with_schema, match_recording_players):
        """Test recording multiple matches and accumulating wins/losses."""
        team1_ids = match_recording_players[:5]
        team2_ids = match_recording_players[5:]

        # Record 3 matches - team 1 wins first two, team 2 wins third
        test_db_with_schema.record_match(team1_ids, team2_ids, winning_team=1)
        test_db_with_schema.record_match(team1_ids, team2_ids, winning_team=1)
        test_db_with_schema.record_match(team1_ids, team2_ids, winning_team=2)

        # Check accumulated stats
        for pid in team1_ids:
            player = test_db_with_schema.get_player(pid)
            assert player.wins == 2
            assert player.losses == 1

        for pid in team2_ids:
            player = test_db_with_schema.get_player(pid)
            assert player.wins == 1
            assert player.losses == 2

    def test_record_match_participants(self, test_db_with_schema, match_recording_players):
        """Test that match participants are correctly recorded."""
        team1_ids = match_recording_players[:5]
        team2_ids = match_recording_players[5:]

        match_id = test_db_with_schema.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        # Check participants in database
        conn = test_db_with_schema.get_connection()
        cursor = conn.cursor()

        # Check team 1 participants
        cursor.execute(
            """
            SELECT discord_id, team_number, won
            FROM match_participants
            WHERE match_id = ? AND team_number = 1
        """,
            (match_id,),
        )
        team1_participants = cursor.fetchall()
        assert len(team1_participants) == 5
        for pid, team_num, won in team1_participants:
            assert pid in team1_ids
            assert team_num == 1
            assert won == 1  # Team 1 won

        # Check team 2 participants
        cursor.execute(
            """
            SELECT discord_id, team_number, won
            FROM match_participants
            WHERE match_id = ? AND team_number = 2
        """,
            (match_id,),
        )
        team2_participants = cursor.fetchall()
        assert len(team2_participants) == 5
        for pid, team_num, won in team2_participants:
            assert pid in team2_ids
            assert team_num == 2
            assert won == 0  # Team 2 lost

        conn.close()


# Edge-case win/loss tests using the radiant/dire kwargs API.

@pytest.fixture
def win_loss_player_ids(test_db_with_schema):
    """Create 10 players (11001-11010) for win/loss edge-case tests."""
    ids = list(range(11001, 11011))
    for pid in ids:
        test_db_with_schema.add_player(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return ids


def _fetch_wins_losses(db, discord_id):
    player = db.get_player(discord_id)
    return player.wins, player.losses


@pytest.mark.parametrize(
    ("winning_team", "winning_slice", "losing_slice"),
    [
        ("radiant", slice(0, 5), slice(5, 10)),
        ("dire", slice(5, 10), slice(0, 5)),
    ],
    ids=["radiant_wins", "dire_wins"],
)
def test_win_updates_wins_and_losses(
    test_db_with_schema, win_loss_player_ids, winning_team, winning_slice, losing_slice
):
    radiant = win_loss_player_ids[:5]
    dire = win_loss_player_ids[5:]

    test_db_with_schema.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team=winning_team,
    )

    for pid in win_loss_player_ids[winning_slice]:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 1
        assert losses == 0

    for pid in win_loss_player_ids[losing_slice]:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 0
        assert losses == 1


def test_multiple_matches_accumulate_correctly(test_db_with_schema, win_loss_player_ids):
    radiant = win_loss_player_ids[:5]
    dire = win_loss_player_ids[5:]

    # Radiant wins twice, Dire wins once
    test_db_with_schema.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant")
    test_db_with_schema.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant")
    test_db_with_schema.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire")

    for pid in radiant:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 2
        assert losses == 1

    for pid in dire:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 1
        assert losses == 2


def test_existing_wins_losses_are_incremented(test_db_with_schema, win_loss_player_ids):
    radiant = win_loss_player_ids[:5]
    dire = win_loss_player_ids[5:]

    # Seed some prior stats
    with test_db_with_schema.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE players SET wins = 2, losses = 3 WHERE discord_id IN ({})".format(
                ",".join("?" * len(radiant))
            ),
            radiant,
        )
        cursor.execute(
            "UPDATE players SET wins = 1, losses = 4 WHERE discord_id IN ({})".format(
                ",".join("?" * len(dire))
            ),
            dire,
        )

    test_db_with_schema.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire")

    for pid in radiant:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 2  # unchanged wins
        assert losses == 4  # incremented loss

    for pid in dire:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 2  # incremented win
        assert losses == 4  # unchanged losses


def test_participants_table_has_correct_side_and_won_flags(test_db_with_schema, win_loss_player_ids):
    radiant = win_loss_player_ids[:5]
    dire = win_loss_player_ids[5:]

    match_id = test_db_with_schema.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team="radiant",
    )

    conn = test_db_with_schema.get_connection()
    cursor = conn.cursor()
    cursor.execute(
        "SELECT discord_id, team_number, won, side FROM match_participants WHERE match_id = ?",
        (match_id,),
    )
    rows = cursor.fetchall()
    conn.close()

    assert len(rows) == 10
    radiant_rows = [row for row in rows if row["discord_id"] in radiant]
    dire_rows = [row for row in rows if row["discord_id"] in dire]

    assert all(row["team_number"] == 1 for row in radiant_rows)
    assert all(row["side"] == "radiant" for row in radiant_rows)
    assert all(row["won"] == 1 for row in radiant_rows)

    assert all(row["team_number"] == 2 for row in dire_rows)
    assert all(row["side"] == "dire" for row in dire_rows)
    assert all(row["won"] == 0 for row in dire_rows)


# =============================================================================
# === Radiant/Dire side ===
# =============================================================================


class TestRadiantDireMapping:
    """Test the Radiant/Dire mapping fix for match recording."""

    def test_radiant_dire_team_mapping(self):
        """Test that Radiant/Dire teams are correctly mapped to team1/team2."""
        # Simulate the shuffle output structure
        # After shuffle, teams are randomly assigned Radiant/Dire
        # We need to ensure wins/losses are recorded correctly

        # Scenario: Team 1 (original) becomes Radiant, Team 2 becomes Dire
        # If Radiant wins, team1_ids should win
        # If Dire wins, team2_ids should win

        radiant_team_num = 1
        dire_team_num = 2

        # If Radiant won
        if radiant_team_num == 1:
            # Radiant is team 1, so team 1 wins
            winning_team_for_db = 1
        else:
            # Radiant is team 2, so team 2 wins
            winning_team_for_db = 2

        # Verify logic
        assert winning_team_for_db == 1  # In this scenario

        # Scenario: Team 1 becomes Dire, Team 2 becomes Radiant
        radiant_team_num = 2
        dire_team_num = 1

        # If Dire won
        if dire_team_num == 1:
            # Dire is team 1, so team 1 wins
            winning_team_for_db = 1
        else:
            # Dire is team 2, so team 2 wins
            winning_team_for_db = 2

        assert winning_team_for_db == 1  # In this scenario

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_team_id_mapping_after_shuffle(self, test_db):
        """
        Test the critical bug fix where team IDs were incorrectly mapped after shuffling.

        The bug was: After shuffling, we assumed player_ids[:5] = team1 and player_ids[5:] = team2,
        but the shuffled teams don't match that order. This test verifies that players are correctly
        mapped by name (as the fix does) rather than by position in the player_ids list.
        """
        # Create 10 players with specific names and Discord IDs
        # The order of player_ids doesn't match the shuffled teams
        player_ids = [4001, 4002, 4003, 4004, 4005, 4006, 4007, 4008, 4009, 4010]
        player_names = [f"Player{i}" for i in range(1, 11)]

        # Add players to database
        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Get Player objects from database
        players = test_db.get_players_by_ids(player_ids)

        # Simulate a shuffle where teams don't match the order of player_ids
        # Team 1: players at positions 0, 2, 4, 6, 8 (Player1, Player3, Player5, Player7, Player9)
        # Team 2: players at positions 1, 3, 5, 7, 9 (Player2, Player4, Player6, Player8, Player10)
        team1_players = [players[0], players[2], players[4], players[6], players[8]]
        team2_players = [players[1], players[3], players[5], players[7], players[9]]

        # Create Team objects
        team1 = Team(team1_players, role_assignments=["1", "2", "3", "4", "5"])
        team2 = Team(team2_players, role_assignments=["1", "2", "3", "4", "5"])

        # Simulate the fix: Map players by name, not by position
        # This is what the fixed code does: player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}

        # Map team1 and team2 players to their Discord IDs (the fix)
        team1_ids = [player_name_to_id[p.name] for p in team1.players]
        team2_ids = [player_name_to_id[p.name] for p in team2.players]

        # Verify the mapping is correct (not just player_ids[:5] and player_ids[5:])
        # Team 1 should have: 4001, 4003, 4005, 4007, 4009
        expected_team1_ids = [4001, 4003, 4005, 4007, 4009]
        # Team 2 should have: 4002, 4004, 4006, 4008, 4010
        expected_team2_ids = [4002, 4004, 4006, 4008, 4010]

        assert set(team1_ids) == set(expected_team1_ids), (
            f"Team 1 IDs don't match! Got {team1_ids}, expected {expected_team1_ids}"
        )
        assert set(team2_ids) == set(expected_team2_ids), (
            f"Team 2 IDs don't match! Got {team2_ids}, expected {expected_team2_ids}"
        )

        # Simulate Radiant/Dire assignment: Team 1 = Radiant, Team 2 = Dire
        radiant_team_ids = team1_ids
        dire_team_ids = team2_ids

        # Record match: Radiant (Team 1) wins
        # Map to database format: team1 = Radiant, team2 = Dire, winning_team = 1
        match_id = test_db.record_match(
            team1_ids=radiant_team_ids, team2_ids=dire_team_ids, winning_team=1
        )

        assert match_id is not None

        # Verify ONLY the correct 5 players got wins (Team 1 / Radiant)
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Player {pid} (Team 1) should have 1 win, got {player.wins}"
            assert player.losses == 0, (
                f"Player {pid} (Team 1) should have 0 losses, got {player.losses}"
            )

        # Verify ONLY the correct 5 players got losses (Team 2 / Dire)
        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Player {pid} (Team 2) should have 0 wins, got {player.wins}"
            assert player.losses == 1, (
                f"Player {pid} (Team 2) should have 1 loss, got {player.losses}"
            )

        # Verify all players are accounted for and have correct win/loss counts
        all_player_ids = set(player_ids)
        team1_set = set(team1_ids)
        team2_set = set(team2_ids)

        # Verify all players are in exactly one team
        assert len(team1_set) == 5, f"Team 1 should have 5 players, got {len(team1_set)}"
        assert len(team2_set) == 5, f"Team 2 should have 5 players, got {len(team2_set)}"
        assert team1_set.isdisjoint(team2_set), "Teams should not share players"
        assert team1_set.union(team2_set) == all_player_ids, "All players should be in a team"

        # Verify win/loss counts for all players
        for pid in all_player_ids:
            player = test_db.get_player(pid)
            if pid in team1_set:
                # Should have 1 win, 0 losses
                assert player.wins == 1 and player.losses == 0, (
                    f"Player {pid} (Team 1) should have 1 win, 0 losses, got {player.wins}-{player.losses}"
                )
            else:  # pid in team2_set
                # Should have 0 wins, 1 loss
                assert player.wins == 0 and player.losses == 1, (
                    f"Player {pid} (Team 2) should have 0 wins, 1 loss, got {player.wins}-{player.losses}"
                )


class TestRadiantDireBugFix:
    """Test the critical bug fix where Dire wins were incorrectly recorded as losses."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_exact_bug_scenario_dire_wins(self, test_db):
        """
        Test the exact bug scenario reported by user.

        Scenario:
        - Radiant: FakeUser917762, FakeUser924119, FakeUser926408, FakeUser921765, FakeUser925589
        - Dire: FakeUser923487, BugReporter, FakeUser921510, FakeUser920053, FakeUser919197
        - Dire won
        - Bug: Dire players were recorded as losses, Radiant players as wins
        - Expected: Dire players should have wins, Radiant players should have losses
        """
        # Create players with exact names from the bug report
        radiant_players = [
            ("FakeUser917762", 1405),
            ("FakeUser924119", 1120),
            ("FakeUser926408", 1763),
            ("FakeUser921765", 1689),
            ("FakeUser925589", 1568),
        ]

        dire_players = [
            ("FakeUser923487", 1161),
            ("BugReporter", 1500),  # The user who reported the bug
            ("FakeUser921510", 1816),
            ("FakeUser920053", 1500),
            ("FakeUser919197", 1601),
        ]

        # Assign Discord IDs (using sequential IDs for testing)
        player_data = []
        discord_id = 90001

        for name, rating in radiant_players + dire_players:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_data.append((discord_id, name, rating))
            discord_id += 1

        # Extract Discord IDs for each team
        radiant_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in radiant_players]
        ]
        dire_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in dire_players]
        ]

        # Verify we have the right players
        assert len(radiant_team_ids) == 5, (
            f"Expected 5 Radiant players, got {len(radiant_team_ids)}"
        )
        assert len(dire_team_ids) == 5, f"Expected 5 Dire players, got {len(dire_team_ids)}"

        # Simulate the shuffle output structure (as stored in bot.last_shuffle)
        # In the actual bug, radiant_team_num could be 1 or 2, dire_team_num would be the other
        # Let's test both scenarios to ensure the fix works

        # Scenario 1: Radiant is team 1, Dire is team 2
        radiant_team_num = 1
        dire_team_num = 2

        # Simulate recording match with "Dire won"
        # This is what the fixed code does:
        winning_team_num = dire_team_num  # Dire won

        # Map winning team to team1/team2 for database
        if winning_team_num == radiant_team_num:
            # Radiant won
            team1_ids_for_db = radiant_team_ids
            team2_ids_for_db = dire_team_ids
            winning_team_for_db = 1
        elif winning_team_num == dire_team_num:
            # Dire won - THIS IS THE BUG SCENARIO
            team1_ids_for_db = dire_team_ids  # Dire goes to team1
            team2_ids_for_db = radiant_team_ids  # Radiant goes to team2
            winning_team_for_db = 1  # team1 (Dire) won
        else:
            raise ValueError(f"Invalid winning_team_num: {winning_team_num}")

        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # CRITICAL TEST: Verify Dire players (who won) have WINS
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            player_name = player.name if player else f"Unknown({pid})"
            assert player.wins == 1, (
                f"BUG REPRODUCTION: Dire player {player_name} (ID: {pid}) should have 1 win, got {player.wins}"
            )
            assert player.losses == 0, (
                f"BUG REPRODUCTION: Dire player {player_name} (ID: {pid}) should have 0 losses, got {player.losses}"
            )

        # CRITICAL TEST: Verify Radiant players (who lost) have LOSSES
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            player_name = player.name if player else f"Unknown({pid})"
            assert player.wins == 0, (
                f"BUG REPRODUCTION: Radiant player {player_name} (ID: {pid}) should have 0 wins, got {player.wins}"
            )
            assert player.losses == 1, (
                f"BUG REPRODUCTION: Radiant player {player_name} (ID: {pid}) should have 1 loss, got {player.losses}"
            )

        # Specifically verify BugReporter (the user who reported the bug) has a WIN
        reporter_player = None
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            if player and player.name == "BugReporter":
                reporter_player = player
                break

        assert reporter_player is not None, "BugReporter player not found in Dire team"
        assert reporter_player.wins == 1, (
            f"BUG: BugReporter should have 1 win (Dire won), got {reporter_player.wins}"
        )
        assert reporter_player.losses == 0, (
            f"BUG: BugReporter should have 0 losses (Dire won), got {reporter_player.losses}"
        )

    def test_exact_bug_scenario_radiant_wins(self, test_db):
        """
        Test the reverse scenario: Radiant wins (to ensure fix works both ways).

        Same teams as bug report, but Radiant wins this time.
        """
        # Same player setup
        radiant_players = [
            ("FakeUser917762", 1405),
            ("FakeUser924119", 1120),
            ("FakeUser926408", 1763),
            ("FakeUser921765", 1689),
            ("FakeUser925589", 1568),
        ]

        dire_players = [
            ("FakeUser923487", 1161),
            ("BugReporter", 1500),
            ("FakeUser921510", 1816),
            ("FakeUser920053", 1500),
            ("FakeUser919197", 1601),
        ]

        player_data = []
        discord_id = 91001

        for name, rating in radiant_players + dire_players:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_data.append((discord_id, name, rating))
            discord_id += 1

        radiant_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in radiant_players]
        ]
        dire_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in dire_players]
        ]

        # Scenario: Radiant is team 2, Dire is team 1 (opposite of previous test)
        radiant_team_num = 2
        dire_team_num = 1

        # Radiant won
        winning_team_num = radiant_team_num

        # Map winning team to team1/team2 for database
        if winning_team_num == radiant_team_num:
            # Radiant won
            team1_ids_for_db = radiant_team_ids
            team2_ids_for_db = dire_team_ids
            winning_team_for_db = 1
        elif winning_team_num == dire_team_num:
            # Dire won
            team1_ids_for_db = dire_team_ids
            team2_ids_for_db = radiant_team_ids
            winning_team_for_db = 1
        else:
            raise ValueError(f"Invalid winning_team_num: {winning_team_num}")

        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # Verify Radiant players (who won) have WINS
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, (
                f"Radiant player {player.name} should have 1 win, got {player.wins}"
            )
            assert player.losses == 0, (
                f"Radiant player {player.name} should have 0 losses, got {player.losses}"
            )

        # Verify Dire players (who lost) have LOSSES
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, (
                f"Dire player {player.name} should have 0 wins, got {player.wins}"
            )
            assert player.losses == 1, (
                f"Dire player {player.name} should have 1 loss, got {player.losses}"
            )

    def test_team_number_validation(self, test_db):
        """Test that the fix properly validates team numbers."""
        # Create test players
        player_ids = list(range(92001, 92011))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        team1_ids = player_ids[:5]
        team2_ids = player_ids[5:]

        # Test with missing team numbers (should be handled gracefully)
        # This simulates what happens if last_shuffle is missing team numbers
        # The fix should handle this by using explicit checks

        # Normal case: both team numbers set
        radiant_team_num = 1
        dire_team_num = 2

        # Dire wins
        winning_team_num = dire_team_num

        # This is the fixed logic
        if radiant_team_num is not None and dire_team_num is not None:
            if radiant_team_num == dire_team_num:
                raise ValueError("Invalid: both teams have same number")

            if winning_team_num == radiant_team_num:
                team1_ids_for_db = team1_ids  # Assuming these are radiant
                team2_ids_for_db = team2_ids  # Assuming these are dire
                winning_team_for_db = 1
            elif winning_team_num == dire_team_num:
                team1_ids_for_db = team2_ids  # Dire goes to team1
                team2_ids_for_db = team1_ids  # Radiant goes to team2
                winning_team_for_db = 1
            else:
                raise ValueError(f"Invalid winning_team_num: {winning_team_num}")
        else:
            raise ValueError("Missing team numbers")

        # Record match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # Verify team2_ids (Radiant) lost
        for pid in team1_ids:
            player = test_db.get_player(pid)
            # These were originally team1 (Radiant), but got swapped to team2, so they lost
            assert player.losses == 1, f"Player {pid} should have 1 loss"

        # Verify team1_ids (Dire) won (they were swapped to team1)
        for pid in team2_ids:
            player = test_db.get_player(pid)
            # These were originally team2 (Dire), but got swapped to team1, so they won
            assert player.wins == 1, f"Player {pid} should have 1 win"


# =============================================================================
# === Betting integration ===
# =============================================================================


class TestBettingEndToEnd:
    """End-to-end coverage for jopacoin wagers."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create test players in the database."""
        player_repo = PlayerRepository(test_db.db_path)
        return _seed_repo_players(player_repo, [1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010])

    def test_bets_settle_with_house(self, test_db, test_players):
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        player_ids = test_players[:10]
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        participant = pending["radiant_team_ids"][0]
        spectator = 9000
        player_repo.add(
            discord_id=spectator,
            discord_username="Spectator",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        player_repo.add_balance(participant, TEST_GUILD_ID, 20)
        player_repo.add_balance(spectator, TEST_GUILD_ID, 10)

        betting_service.place_bet(TEST_GUILD_ID, participant, "radiant", 5, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectator, "dire", 5, pending)

        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert "bet_distributions" in result
        distributions = result["bet_distributions"]
        assert distributions["winners"], "Expected at least one winning distribution"
        assert distributions["winners"][0]["discord_id"] == participant
        assert distributions["losers"][0]["discord_id"] == spectator

        expected_participant_balance = 3 + 20 - 5 + 10 + JOPACOIN_WIN_REWARD
        assert player_repo.get_balance(participant, TEST_GUILD_ID) == expected_participant_balance
        # Spectator starts with 3, gets +10 top-up, -5 lost bet = 8
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 8

    def test_excluded_players_receive_exclusion_bonus(self, test_db):
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        player_ids = list(range(5101, 5113))  # 12 players -> 2 excluded
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        excluded_ids = pending["excluded_player_ids"]
        assert len(excluded_ids) == 2

        # Start everyone at zero for deterministic balance checks
        for pid in player_ids:
            player_repo.update_balance(pid, TEST_GUILD_ID, 0)

        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        for pid in excluded_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == JOPACOIN_EXCLUSION_REWARD

        included_ids = set(player_ids) - set(excluded_ids)
        for pid in included_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) != JOPACOIN_EXCLUSION_REWARD

    def test_max_lobby_14_players_4_excluded(self, test_db):
        """
        Test max lobby scenario: 14 players with 4 excluded.

        This tests the maximum lobby size to ensure all excluded players
        are correctly tracked and receive exclusion bonuses.
        """
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=True,
        )

        # Mix of positive IDs (real users) and negative IDs (fake users)
        real_ids = [6001, 6002, 6003, 6004]
        fake_ids = list(range(-1, -11, -1))  # -1 through -10
        player_ids = real_ids + fake_ids  # 14 total

        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{abs(pid)}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
                preferred_roles=["1", "2", "3", "4", "5"],
            )

        result = match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        excluded_ids = result["excluded_ids"]

        # With 14 players and 10 selected, exactly 4 must be excluded
        assert len(excluded_ids) == 4, f"Expected 4 excluded, got {len(excluded_ids)}"

        # All excluded IDs must be valid player IDs
        for pid in excluded_ids:
            assert pid in player_ids, f"Excluded ID {pid} not in player_ids"
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player is not None, f"Player {pid} not found in repo"

        # Verify exclusion IDs are stored in pending state
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert pending["excluded_player_ids"] == excluded_ids

        # Verify radiant + dire + excluded = all players
        all_in_match = set(pending["radiant_team_ids"] + pending["dire_team_ids"])
        all_excluded = set(excluded_ids)
        assert len(all_in_match) == 10
        assert len(all_excluded) == 4
        assert all_in_match.isdisjoint(all_excluded), "Excluded player found in match teams"
        assert all_in_match | all_excluded == set(player_ids)

    def test_betting_totals_display_correctly_after_previous_match(self, test_db, test_players):
        """
        E2E test for the betting totals display bug fix.

        Scenario: User bets 6 jopacoin on Dire, but it shows as 3 because
        previous settled bets are being counted. This test verifies the fix.
        """
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        # First match: Create and settle with some bets
        player_ids_match1 = test_players[:10]
        match_service.shuffle_players(player_ids_match1, guild_id=TEST_GUILD_ID)
        pending1 = match_service.get_last_shuffle(TEST_GUILD_ID)

        spectator1 = 9001
        spectator2 = 9002
        player_repo.add(
            discord_id=spectator1,
            discord_username="Spectator1",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.add(
            discord_id=spectator2,
            discord_username="Spectator2",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        player_repo.add_balance(spectator1, TEST_GUILD_ID, 20)
        player_repo.add_balance(spectator2, TEST_GUILD_ID, 20)

        # Place bets on first match: 3 on radiant, 2 on dire
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 3, pending1)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 2, pending1)

        # Verify totals show pending bets correctly
        totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending1)
        assert totals["radiant"] == 3, "Should show 3 jopacoin on Radiant"
        assert totals["dire"] == 2, "Should show 2 jopacoin on Dire"

        # Settle the first match (this assigns match_id to the bets)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # After settling, totals should be 0 (no pending bets)
        totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending1)
        assert totals["radiant"] == 0, "Should show 0 after settling (no pending bets)"
        assert totals["dire"] == 0, "Should show 0 after settling (no pending bets)"

        # Second match: Create new match and place new bets
        player_ids_match2 = [2001, 2002, 2003, 2004, 2005, 2006, 2007, 2008, 2009, 2010]
        for pid in player_ids_match2:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        match_service.shuffle_players(player_ids_match2, guild_id=TEST_GUILD_ID)
        pending2 = match_service.get_last_shuffle(TEST_GUILD_ID)

        # User bets 6 jopacoin on Dire (the exact bug scenario)
        spectator3 = 9003
        player_repo.add(
            discord_id=spectator3,
            discord_username="Spectator3",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1100,
            glicko_rating=1100.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.add_balance(spectator3, TEST_GUILD_ID, 20)

        betting_service.place_bet(TEST_GUILD_ID, spectator3, "dire", 6, pending2)

        # CRITICAL: Verify totals only show the new pending bet (6), not old settled bets
        # Before the fix, this would show 3 (6 - 3 from previous match, or some incorrect calculation)
        totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending2)
        assert totals["radiant"] == 0, "Should show 0 on Radiant (no pending bets)"
        assert totals["dire"] == 6, (
            f"Should show 6 jopacoin on Dire (the bet just placed), got {totals['dire']}"
        )

        # Verify the bet was recorded correctly
        bet = bet_repo.get_player_pending_bet(TEST_GUILD_ID, spectator3, since_ts=pending2["shuffle_timestamp"])
        assert bet is not None, "Bet should exist"
        assert bet["amount"] == 6, "Bet amount should be 6"
        assert bet["team_bet_on"] == "dire", "Bet should be on Dire"

    def test_betting_totals_multiple_bets_same_match(self, test_db, test_players):
        """
        E2E test: Multiple users place bets on the same match, verify totals are correct.
        """
        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        betting_service = BettingService(bet_repo, player_repo)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
        )

        player_ids = test_players[:10]
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        # Create spectators
        spectators = []
        for i in range(4):
            spectator_id = 9100 + i
            player_repo.add(
                discord_id=spectator_id,
                discord_username=f"Spectator{i}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1100,
                glicko_rating=1100.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_repo.add_balance(spectator_id, TEST_GUILD_ID, 20)
            spectators.append(spectator_id)

        # Place multiple bets
        betting_service.place_bet(TEST_GUILD_ID, spectators[0], "radiant", 5, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectators[1], "radiant", 3, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectators[2], "dire", 4, pending)
        betting_service.place_bet(TEST_GUILD_ID, spectators[3], "dire", 6, pending)

        # Verify totals are correct
        totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending)
        assert totals["radiant"] == 8, (
            f"Should show 8 jopacoin on Radiant (5+3), got {totals['radiant']}"
        )
        assert totals["dire"] == 10, f"Should show 10 jopacoin on Dire (4+6), got {totals['dire']}"


class TestLoanRepaymentOnMatchRecord:
    """End-to-end tests for loan repayment when matches are recorded."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def services(self, test_db):
        """Create all required services with loan integration."""
        from repositories.loan_repository import LoanRepository
        from services.loan_service import LoanService

        player_repo = PlayerRepository(test_db.db_path)
        bet_repo = BetRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        loan_repo = LoanRepository(test_db.db_path)

        betting_service = BettingService(bet_repo, player_repo)
        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
        )
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=betting_service,
            loan_service=loan_service,
        )

        return {
            "player_repo": player_repo,
            "bet_repo": bet_repo,
            "match_repo": match_repo,
            "loan_repo": loan_repo,
            "betting_service": betting_service,
            "loan_service": loan_service,
            "match_service": match_service,
            "db": test_db,
        }

    @pytest.fixture
    def test_players(self, services):
        """Create 10 test players."""
        player_repo = services["player_repo"]
        player_ids = [3001, 3002, 3003, 3004, 3005, 3006, 3007, 3008, 3009, 3010]
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return player_ids

    def test_loan_repaid_when_borrower_wins(self, services, test_players):
        """Loan is repaid when borrower participates in a match (winning team)."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        # Borrower starts with 0 balance (after default 3)
        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, TEST_GUILD_ID, 0)

        # Take a loan of 50 (fee = 10, total owed = 60)
        result = loan_service.execute_loan(borrower_id, 50, guild_id=TEST_GUILD_ID)
        assert result.success
        assert player_repo.get_balance(borrower_id, TEST_GUILD_ID) == 50  # Got full loan amount

        # Verify outstanding loan exists
        state = loan_service.get_state(borrower_id, guild_id=TEST_GUILD_ID)
        assert state.has_outstanding_loan
        assert state.outstanding_principal == 50
        assert state.outstanding_fee == 10

        # Shuffle and record match (borrower is on radiant, radiant wins)
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        # Make sure borrower is on radiant for predictable test
        if borrower_id not in pending["radiant_team_ids"]:
            # Swap teams so borrower is on winning side
            pending["radiant_team_ids"], pending["dire_team_ids"] = (
                pending["dire_team_ids"],
                pending["radiant_team_ids"],
            )
            # Persist the swapped state
            match_service._persist_match_state(TEST_GUILD_ID, pending)

        # Record radiant win
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Loan should be repaid
        state = loan_service.get_state(borrower_id, guild_id=TEST_GUILD_ID)
        assert not state.has_outstanding_loan
        assert state.outstanding_principal == 0
        assert state.outstanding_fee == 0

        # Balance: started 0, +50 loan, -60 repayment, +win_reward = -10 + win_reward
        expected = 50 - 60 + JOPACOIN_WIN_REWARD
        assert player_repo.get_balance(borrower_id, TEST_GUILD_ID) == expected

    def test_loan_repaid_when_borrower_loses(self, services, test_players):
        """Loan is repaid even when borrower loses the match."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, TEST_GUILD_ID, 0)

        # Take loan of 50
        loan_service.execute_loan(borrower_id, 50, guild_id=TEST_GUILD_ID)
        assert player_repo.get_balance(borrower_id, TEST_GUILD_ID) == 50

        # Shuffle match
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        # Make sure borrower is on dire (losing side)
        if borrower_id not in pending["dire_team_ids"]:
            pending["radiant_team_ids"], pending["dire_team_ids"] = (
                pending["dire_team_ids"],
                pending["radiant_team_ids"],
            )
            # Persist the swapped state
            match_service._persist_match_state(TEST_GUILD_ID, pending)

        # Radiant wins (borrower loses)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Loan still repaid
        state = loan_service.get_state(borrower_id, guild_id=TEST_GUILD_ID)
        assert not state.has_outstanding_loan

        # Balance: 0 + 50 loan - 60 repayment + 1 participation = -9 (debt)
        assert player_repo.get_balance(borrower_id, TEST_GUILD_ID) == -9

    def test_loan_not_repaid_if_not_participant(self, services, test_players):
        """Loan is NOT repaid if borrower doesn't participate in the match."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        # Create a non-participant who takes a loan
        spectator_id = 9999
        player_repo.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.update_balance(spectator_id, TEST_GUILD_ID, 0)

        # Spectator takes loan
        loan_service.execute_loan(spectator_id, 50, guild_id=TEST_GUILD_ID)
        assert player_repo.get_balance(spectator_id, TEST_GUILD_ID) == 50

        # Match is played by different players
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Spectator's loan should NOT be repaid
        state = loan_service.get_state(spectator_id, guild_id=TEST_GUILD_ID)
        assert state.has_outstanding_loan
        assert state.outstanding_principal == 50
        assert player_repo.get_balance(spectator_id, TEST_GUILD_ID) == 50  # Unchanged

    def test_loan_with_spectator_bet(self, services, test_players):
        """Spectator with loan bets on the match and loan repaid next game."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        betting_service = services["betting_service"]
        match_service = services["match_service"]

        # Create a spectator who takes a loan
        spectator_id = 9998
        player_repo.add(
            discord_id=spectator_id,
            discord_username="Spectator",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.update_balance(spectator_id, TEST_GUILD_ID, 0)

        # Spectator takes loan of 100 (fee = 20)
        loan_service.execute_loan(spectator_id, 100, guild_id=TEST_GUILD_ID)
        assert player_repo.get_balance(spectator_id, TEST_GUILD_ID) == 100

        # Match 1: spectator bets but doesn't play (house mode for 1:1 payout)
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID, betting_mode="house")
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        # Spectator bets 30 on radiant
        betting_service.place_bet(TEST_GUILD_ID, spectator_id, "radiant", 30, pending)
        assert player_repo.get_balance(spectator_id, TEST_GUILD_ID) == 70  # 100 - 30

        # Record radiant win
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Spectator won the bet: +30 back + 30 winnings = +60
        # But loan NOT repaid (spectator didn't participate)
        state = loan_service.get_state(spectator_id, guild_id=TEST_GUILD_ID)
        assert state.has_outstanding_loan
        assert player_repo.get_balance(spectator_id, TEST_GUILD_ID) == 130  # 70 + 60

        # Match 2: Now spectator plays and loan is repaid
        new_players = test_players[:9] + [spectator_id]  # Replace one player with spectator
        match_service.shuffle_players(new_players, guild_id=TEST_GUILD_ID, betting_mode="house")
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Loan repaid now
        state = loan_service.get_state(spectator_id, guild_id=TEST_GUILD_ID)
        assert not state.has_outstanding_loan

        # Balance: 130 - 120 repayment + (1 if lost, 2 if won)
        balance = player_repo.get_balance(spectator_id, TEST_GUILD_ID)
        # Winners get +2 (win bonus), losers get +1 (participation)
        assert balance in [11, 12]  # 130 - 120 + 1 = 11, or 130 - 120 + 2 = 12

    def test_loan_repayment_pushes_into_debt(self, services, test_players):
        """Loan repayment can push player into debt when they've spent the money."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, TEST_GUILD_ID, 0)

        # Take max loan of 100 (fee = 20, total owed = 120)
        loan_service.execute_loan(borrower_id, 100, guild_id=TEST_GUILD_ID)
        assert player_repo.get_balance(borrower_id, TEST_GUILD_ID) == 100

        # "Spend" the loan money (set balance to 0)
        player_repo.update_balance(borrower_id, TEST_GUILD_ID, 0)

        # Play a match
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Loan repaid, player in debt
        state = loan_service.get_state(borrower_id, guild_id=TEST_GUILD_ID)
        assert not state.has_outstanding_loan

        # Balance: 0 - 120 + (1 if lost, 2 if won)
        balance = player_repo.get_balance(borrower_id, TEST_GUILD_ID)
        assert balance in [-119, -118]  # Lost: -119, Won: -118

    def test_multiple_players_with_loans(self, services, test_players):
        """Multiple players have loans, all repaid after match."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        # Two players take loans
        borrower1 = test_players[0]
        borrower2 = test_players[5]

        player_repo.update_balance(borrower1, TEST_GUILD_ID, 0)
        player_repo.update_balance(borrower2, TEST_GUILD_ID, 0)

        loan_service.execute_loan(borrower1, 50, guild_id=TEST_GUILD_ID)  # owes 60
        loan_service.execute_loan(borrower2, 100, guild_id=TEST_GUILD_ID)  # owes 120

        # Shuffle and record
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Both loans repaid
        assert not loan_service.get_state(borrower1, guild_id=TEST_GUILD_ID).has_outstanding_loan
        assert not loan_service.get_state(borrower2, guild_id=TEST_GUILD_ID).has_outstanding_loan

        # Balances: loan - repayment + reward
        # Winners get +2 (JOPACOIN_WIN_REWARD), losers get +1 (participation)
        # Borrower1: 50 - 60 + (2 if won, 1 if lost)
        # Borrower2: 100 - 120 + (2 if won, 1 if lost)
        b1_balance = player_repo.get_balance(borrower1, TEST_GUILD_ID)
        b2_balance = player_repo.get_balance(borrower2, TEST_GUILD_ID)

        # If won: loan - repayment + win_reward
        # If lost: loan - repayment + participation
        assert b1_balance in [-9, -8]  # 50 - 60 + 1 = -9, or 50 - 60 + 2 = -8
        assert b2_balance in [-19, -18]  # 100 - 120 + 1 = -19, or 100 - 120 + 2 = -18

    def test_loan_repayment_fee_goes_to_nonprofit(self, services, test_players):
        """Verify the loan fee is added to nonprofit fund on repayment."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        loan_repo = services["loan_repo"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, TEST_GUILD_ID, 0)

        # Get nonprofit fund before
        nonprofit_before = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)

        # Take loan of 100 (fee = 20)
        loan_service.execute_loan(borrower_id, 100, guild_id=TEST_GUILD_ID)

        # Fee not added yet (deferred)
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == nonprofit_before

        # Play and record match
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Now fee should be in nonprofit fund
        nonprofit_after = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)
        assert nonprofit_after == nonprofit_before + 20

    def test_loan_repayment_result_in_match_record(self, services, test_players):
        """Verify loan repayments are reported in match record result."""
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        match_service = services["match_service"]

        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, TEST_GUILD_ID, 0)
        loan_service.execute_loan(borrower_id, 75, guild_id=TEST_GUILD_ID)  # owes 90

        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Check loan_repayments in result
        assert "loan_repayments" in result
        repayments = result["loan_repayments"]
        assert len(repayments) == 1
        assert repayments[0]["player_id"] == borrower_id
        assert repayments[0]["principal"] == 75
        assert repayments[0]["fee"] == 15
        assert repayments[0]["total_repaid"] == 90


# =============================================================================
# === Admin recording ===
# =============================================================================
# Admin/voting tests share fixtures: a dedicated DB, player repo, match service,
# and per-class player pools (different ID ranges to avoid collisions).


@pytest.fixture
def admin_test_db(repo_db_path):
    """Create a test database using centralized fast fixture."""
    return Database(repo_db_path)


@pytest.fixture
def admin_player_repo(admin_test_db):
    """Create a PlayerRepository instance."""
    return PlayerRepository(admin_test_db.db_path)


@pytest.fixture
def admin_match_service(admin_test_db, admin_player_repo):
    """Create a MatchService instance with Glicko enabled."""
    match_repo = MatchRepository(admin_test_db.db_path)
    return MatchService(player_repo=admin_player_repo, match_repo=match_repo, use_glicko=True)


@pytest.fixture
def admin_test_players(admin_player_repo):
    """Create 10 test players for admin tests."""
    return _seed_repo_players(admin_player_repo, list(range(5001, 5011)))


@pytest.fixture
def voting_test_players(admin_player_repo):
    """Create 10 test players for voting tests."""
    return _seed_repo_players(admin_player_repo, list(range(6001, 6011)))


@pytest.fixture
def abort_test_players(admin_player_repo):
    """Create 10 test players for abort tests."""
    return _seed_repo_players(admin_player_repo, list(range(7001, 7011)))


class TestAdminOverride:
    """Test admin override functionality for match recording."""

    def test_has_admin_submission_with_no_submissions(self, admin_match_service, admin_test_players):
        """Test has_admin_submission returns False when no submissions exist."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False

    def test_has_admin_submission_with_non_admin_submission(
        self, admin_match_service, admin_test_players
    ):
        """Test has_admin_submission returns False when only non-admin submits."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False

    def test_has_admin_submission_with_admin_submission(self, admin_match_service, admin_test_players):
        """Test has_admin_submission returns True when admin submits."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)

        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is True

    def test_can_record_match_with_admin_override(self, admin_match_service, admin_test_players):
        """Test can_record_match returns True with admin override, bypassing non-admin requirement."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Admin submits - should bypass the 3 non-admin requirement
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)

        # Should be ready to record even though non_admin_count is 0
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 0

    def test_can_record_match_without_admin_requires_3_non_admin(
        self, admin_match_service, admin_test_players
    ):
        """Test can_record_match requires 3 non-admin submissions when no admin submits."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Add 2 non-admin submissions - should not be ready
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="radiant", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 2

        # Add 3rd non-admin submission - should be ready
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 3

    def test_admin_override_allows_immediate_recording(self, admin_match_service, admin_test_players):
        """Test that admin submission allows immediate match recording."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Admin submits - should allow immediate recording
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True
        )

        assert submission["is_ready"] is True
        assert submission["non_admin_count"] == 0
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True

        # Should be able to record match immediately
        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert record_result["match_id"] is not None
        assert record_result["winning_team"] == "radiant"
        assert record_result["updated_count"] == 10

    def test_admin_override_with_mixed_submissions(self, admin_match_service, admin_test_players):
        """Test admin override works even when non-admin submissions exist."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Add 1 non-admin submission (not enough)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False

        # Admin submits - should override and allow recording
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True
        )

        assert submission["is_ready"] is True
        assert submission["non_admin_count"] == 1  # Still only 1 non-admin
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True

        # Should be able to record
        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        assert record_result["match_id"] is not None

    def test_admin_override_clears_state_after_recording(
        self, admin_match_service, admin_test_players
    ):
        """Test that state is cleared after admin override recording."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Admin submits and records
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)
        admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # State should be cleared
        assert admin_match_service.get_last_shuffle(TEST_GUILD_ID) is None
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False


class TestFirstToThreeVoting:
    """Test first-to-3 voting system for non-admin match recording."""

    def test_get_vote_counts_empty(self, admin_match_service, voting_test_players):
        """Test get_vote_counts returns zeros when no submissions."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 0, "dire": 0}

    def test_get_vote_counts_tracks_votes(self, admin_match_service, voting_test_players):
        """Test get_vote_counts correctly tracks non-admin votes."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Add some votes
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 2, "dire": 1}

    def test_get_vote_counts_excludes_admin(self, admin_match_service, voting_test_players):
        """Test get_vote_counts does not count admin votes."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Add admin and non-admin votes
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 1, "dire": 0}

    def test_conflicting_votes_allowed(self, admin_match_service, voting_test_players):
        """Test that users can vote for different results (requires MIN_NON_ADMIN_SUBMISSIONS to confirm)."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Add conflicting votes - should not raise
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 2, "dire": 1}

    def test_first_to_3_radiant_wins(self, admin_match_service, voting_test_players):
        """Test that radiant wins when it reaches 3 votes first."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # 2 radiant, 2 dire - not ready
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1004, result="dire", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) is None

        # 3rd radiant vote - radiant wins!
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1005, result="radiant", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) == "radiant"

    def test_first_to_3_dire_wins(self, admin_match_service, voting_test_players):
        """Test that dire wins when it reaches 3 votes first."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # 1 radiant, 2 dire
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="dire", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False

        # 3rd dire vote - dire wins!
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1004, result="dire", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "dire"
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) == "dire"

    def test_user_cannot_change_vote(self, admin_match_service, voting_test_players):
        """Test that a user cannot change their vote."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        # Same user tries to vote differently
        with pytest.raises(ValueError, match="already submitted"):
            admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="dire", is_admin=False)

    def test_user_can_revote_same_result(self, admin_match_service, voting_test_players):
        """Test that a user can submit the same vote again (no-op)."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        # Same vote again - should not raise, just update
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 1, "dire": 0}  # Still just 1 vote

    def test_submission_returns_vote_counts(self, admin_match_service, voting_test_players):
        """Test that add_record_submission returns current vote counts."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False
        )

        assert "vote_counts" in submission
        assert submission["vote_counts"] == {"radiant": 1, "dire": 1}

    def test_first_to_3_records_correct_winner(self, admin_match_service, voting_test_players):
        """Test that the match is recorded with the correct winner."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Radiant gets 3 votes, Dire gets 2
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1004, result="radiant", is_admin=False)
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1005, result="radiant", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"

        # Record the match
        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert record_result["winning_team"] == "radiant"
        assert record_result["match_id"] is not None


class TestAbortVoting:
    """Test abort submission handling for match recording."""

    def test_non_admin_abort_requires_three_votes(self, admin_match_service, abort_test_players):
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False

        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1001, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1002, is_admin=False)

        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False
        submission = admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1003, is_admin=False)
        assert submission["is_ready"] is True
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True

    def test_admin_abort_overrides_minimum(self, admin_match_service, abort_test_players):
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        submission = admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=9999, is_admin=True)

        assert submission["is_ready"] is True
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True
        assert submission["non_admin_count"] == admin_match_service.get_abort_submission_count(TEST_GUILD_ID)

    def test_clear_abort_state_after_abort(self, admin_match_service, abort_test_players):
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1001, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1002, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1003, is_admin=False)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True

        admin_match_service.clear_last_shuffle(TEST_GUILD_ID)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False


# =============================================================================
# === Logging ===
# =============================================================================


class TestMatchRecordingLogging:
    """Test enhanced logging for match recording with player names."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_match_logging_includes_player_names(self, test_db, caplog):
        """Test that match recording logs include player names for winners and losers."""
        # Set up logging capture
        caplog.set_level(logging.INFO)

        # Create test players with distinct names
        player_ids = [5001, 5002, 5003, 5004, 5005, 5006, 5007, 5008, 5009, 5010]
        player_names = [f"Winner{i}" if i <= 5 else f"Loser{i - 5}" for i in range(1, 11)]

        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Split into teams
        team1_ids = player_ids[:5]  # Winners
        team2_ids = player_ids[5:]  # Losers

        # Simulate the match recording logic from bot.py
        # This tests the logging format without needing the full Discord bot
        CamaRatingSystem()

        # Get player names for logging (simulating bot.py logic)
        winning_team_ids = team1_ids
        losing_team_ids = team2_ids

        winning_player_names = []
        losing_player_names = []

        for player_id in winning_team_ids:
            player_obj = test_db.get_player(player_id)
            if player_obj:
                winning_player_names.append(player_obj.name)
            else:
                winning_player_names.append(f"Unknown({player_id})")

        for player_id in losing_team_ids:
            player_obj = test_db.get_player(player_id)
            if player_obj:
                losing_player_names.append(player_obj.name)
            else:
                losing_player_names.append(f"Unknown({player_id})")

        # Record the match
        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        # Simulate the logging that happens in bot.py
        winning_team_display = "Team 1"
        winning_team_num = 1
        updated_count = 10

        log_message = (
            f"Match {match_id} recorded - {winning_team_display} (Team {winning_team_num}) won. "
            f"Updated ratings for {updated_count} players. "
            f"Winners: {', '.join(winning_player_names)}. "
            f"Losers: {', '.join(losing_player_names)}"
        )

        # Log it (simulating what bot.py does)
        logger = logging.getLogger("test")
        logger.info(log_message)

    def test_match_logging_radiant_dire_format(self, test_db, caplog):
        """Test that logging works correctly with Radiant/Dire team names."""
        caplog.set_level(logging.INFO)

        # Create test players
        player_ids = [6001, 6002, 6003, 6004, 6005, 6006, 6007, 6008, 6009, 6010]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Simulate Radiant/Dire scenario
        radiant_team_ids = player_ids[:5]
        dire_team_ids = player_ids[5:]
        winning_team_num = 1  # Radiant won
        winning_team_display = "Radiant"

        # Get player names
        winning_player_names = []
        losing_player_names = []

        for player_id in radiant_team_ids:
            player_obj = test_db.get_player(player_id)
            winning_player_names.append(player_obj.name if player_obj else f"Unknown({player_id})")

        for player_id in dire_team_ids:
            player_obj = test_db.get_player(player_id)
            losing_player_names.append(player_obj.name if player_obj else f"Unknown({player_id})")

        # Record match
        match_id = test_db.record_match(
            team1_ids=radiant_team_ids, team2_ids=dire_team_ids, winning_team=1
        )

        # Simulate logging
        updated_count = 10
        log_message = (
            f"Match {match_id} recorded - {winning_team_display} (Team {winning_team_num}) won. "
            f"Updated ratings for {updated_count} players. "
            f"Winners: {', '.join(winning_player_names)}. "
            f"Losers: {', '.join(losing_player_names)}"
        )

        logger = logging.getLogger("test")
        logger.info(log_message)

        # Verify Radiant is mentioned
        assert "Radiant" in log_message
        assert f"Team {winning_team_num}" in log_message

        # Verify all players are listed
        assert len(winning_player_names) == 5
        assert len(losing_player_names) == 5
        assert all(name in log_message for name in winning_player_names)
        assert all(name in log_message for name in losing_player_names)


# =============================================================================
# === Bug regressions ===
# =============================================================================


class TestExcludedPlayersBug:
    """Test the critical bug where excluded players were getting losses recorded."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_excluded_player_should_not_get_loss(self, test_db):
        """
        Test the exact bug scenario: player was excluded from match but got a loss.

        Scenario:
        - 11 players in lobby
        - 10 players selected for match, 1 excluded (BugReporter)
        - Match recorded with Dire won
        - Bug: BugReporter (excluded) got a loss
        - Expected: BugReporter should have 0 wins, 0 losses (not in match)
        """
        # Create 11 players (10 for match + 1 excluded)
        player_names = [
            "FakeUser172699",
            "FakeUser169817",
            "FakeUser167858",
            "FakeUser175544",
            "FakeUser173967",
            "FakeUser170233",
            "FakeUser174788",
            "FakeUser171621",
            "FakeUser166664",
            "FakeUser168472",
            "BugReporter",  # This player should be excluded
        ]

        player_ids = []
        for idx, name in enumerate(player_names):
            discord_id = 96001 + idx
            player_ids.append(discord_id)
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Simulate shuffle: 10 players in match, 1 excluded
        # First 10 players are in the match
        match_player_ids = player_ids[:10]
        excluded_player_id = player_ids[10]  # BugReporter

        # Split match players into teams
        radiant_team_ids = match_player_ids[:5]
        dire_team_ids = match_player_ids[5:10]

        # Verify excluded player is NOT in match
        assert excluded_player_id not in radiant_team_ids
        assert excluded_player_id not in dire_team_ids
        assert excluded_player_id not in match_player_ids[:10]

        # Record match - Dire won
        # Simulate the fixed logic
        team1_ids_for_db = dire_team_ids  # Dire goes to team1
        team2_ids_for_db = radiant_team_ids  # Radiant goes to team2
        winning_team_for_db = 1  # team1 (Dire) won

        # CRITICAL VALIDATION: Ensure excluded player is NOT in match
        all_match_ids = set(team1_ids_for_db + team2_ids_for_db)
        assert excluded_player_id not in all_match_ids, (
            f"BUG: Excluded player {excluded_player_id} found in match teams!"
        )

        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # CRITICAL TEST: Excluded player should have 0 wins, 0 losses
        excluded_player = test_db.get_player(excluded_player_id)
        assert excluded_player is not None
        assert excluded_player.wins == 0, (
            f"BUG: Excluded player BugReporter should have 0 wins, got {excluded_player.wins}"
        )
        assert excluded_player.losses == 0, (
            f"BUG: Excluded player BugReporter should have 0 losses, got {excluded_player.losses}. "
            f"This is the exact bug that was reported!"
        )

        # Verify match players have correct stats
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Dire player {player.name} should have 1 win"
            assert player.losses == 0, f"Dire player {player.name} should have 0 losses"

        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Radiant player {player.name} should have 0 wins"
            assert player.losses == 1, f"Radiant player {player.name} should have 1 loss"

    def test_excluded_players_validation(self, test_db):
        """Test that validation prevents excluded players from being in match."""
        # Create 11 players
        player_ids = list(range(97001, 97012))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Simulate: 10 players in match, 1 excluded
        match_player_ids = player_ids[:10]
        excluded_player_id = player_ids[10]

        # Split into teams
        team1_ids = match_player_ids[:5]
        team2_ids = match_player_ids[5:10]

        # Verify excluded player is not in teams
        assert excluded_player_id not in team1_ids
        assert excluded_player_id not in team2_ids

        # Record match
        test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        # Verify excluded player has no stats
        excluded_player = test_db.get_player(excluded_player_id)
        assert excluded_player.wins == 0
        assert excluded_player.losses == 0

        # Verify match players have stats
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1


class TestPlayerOrderPreservation:
    """
    Test that get_players_by_ids preserves input order.

    This is critical because the player_name_to_id mapping relies on
    zip(player_ids, players) being in the same order.

    Bug scenario: SQLite returns rows in arbitrary order, causing mismatched
    Discord IDs and Player names, which results in wrong team assignments.
    """

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_get_players_by_ids_preserves_order(self, test_db):
        """
        Test that get_players_by_ids returns players in the SAME order
        as the input discord_ids, regardless of database insertion order.
        """
        # Add players in a specific order
        players_data = [
            (1001, "Alice", 1500),
            (1002, "Bob", 1600),
            (1003, "Charlie", 1700),
            (1004, "Diana", 1800),
            (1005, "Eve", 1900),
        ]

        for discord_id, name, rating in players_data:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Request players in REVERSE order
        requested_ids = [1005, 1003, 1001, 1004, 1002]
        players = test_db.get_players_by_ids(requested_ids)

        # CRITICAL: Players must be returned in the same order as requested
        assert len(players) == 5
        assert players[0].name == "Eve"  # 1005
        assert players[1].name == "Charlie"  # 1003
        assert players[2].name == "Alice"  # 1001
        assert players[3].name == "Diana"  # 1004
        assert players[4].name == "Bob"  # 1002

    def test_player_name_to_id_mapping_correctness(self, test_db):
        """
        Test that the player_name_to_id mapping is correct when using
        get_players_by_ids with zip().

        This is the exact pattern used in bot.py that was causing the bug.
        """
        # Add players
        players_data = [
            (1001, "Alice", 1500),
            (1002, "Bob", 1600),
            (1003, "Charlie", 1700),
        ]

        for discord_id, name, rating in players_data:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Simulate the pattern used in bot.py
        player_ids = [1003, 1001, 1002]  # Not in insertion order
        players = test_db.get_players_by_ids(player_ids)

        # Build the mapping (exact pattern from bot.py)
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}

        # CRITICAL: Mapping must be correct
        assert player_name_to_id["Charlie"] == 1003
        assert player_name_to_id["Alice"] == 1001
        assert player_name_to_id["Bob"] == 1002

    def test_team_assignment_with_shuffled_ids(self, test_db):
        """
        End-to-end test simulating the exact scenario that caused the bug:
        1. Players join lobby (in some order)
        2. get_players_by_ids is called
        3. player_name_to_id mapping is built
        4. Teams are assigned
        5. Match is recorded

        The bug was that team assignments were scrambled because
        get_players_by_ids returned players in a different order than
        the input IDs.
        """
        # Create 10 players
        player_data = [
            (101, "BugReporter", 1500),
            (102, "FakeUser2", 1992),
            (103, "FakeUser7", 1667),
            (104, "TestPlayerA", 1028),
            (105, "FakeUser3", 1078),
            (106, "TestPlayerB", 1021),
            (107, "FakeUser6", 1184),
            (108, "FakeUser4", 1494),
            (109, "FakeUser5", 1759),
            (110, "FakeUser1", 1825),
        ]

        for discord_id, name, rating in player_data:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Simulate lobby order (might be different from insertion order)
        lobby_player_ids = [110, 109, 108, 107, 106, 105, 104, 103, 102, 101]

        # Get players from database
        players = test_db.get_players_by_ids(lobby_player_ids)

        # Build the name-to-id mapping
        player_name_to_id = {pl.name: pid for pid, pl in zip(lobby_player_ids, players)}

        # Verify mapping is correct
        assert player_name_to_id["FakeUser1"] == 110
        assert player_name_to_id["FakeUser5"] == 109
        assert player_name_to_id["FakeUser4"] == 108
        assert player_name_to_id["FakeUser6"] == 107
        assert player_name_to_id["TestPlayerB"] == 106
        assert player_name_to_id["FakeUser3"] == 105
        assert player_name_to_id["TestPlayerA"] == 104
        assert player_name_to_id["FakeUser7"] == 103
        assert player_name_to_id["FakeUser2"] == 102
        assert player_name_to_id["BugReporter"] == 101

        # Simulate team assignment (from shuffle)
        radiant_names = ["BugReporter", "FakeUser2", "FakeUser7", "TestPlayerA", "FakeUser3"]
        dire_names = ["TestPlayerB", "FakeUser6", "FakeUser4", "FakeUser5", "FakeUser1"]

        # Map names to IDs (the critical step that was failing)
        radiant_team_ids = [player_name_to_id[name] for name in radiant_names]
        dire_team_ids = [player_name_to_id[name] for name in dire_names]

        # Verify team IDs are correct
        assert radiant_team_ids == [101, 102, 103, 104, 105]
        assert dire_team_ids == [106, 107, 108, 109, 110]

        # Record match - Radiant won
        test_db.record_match(
            radiant_team_ids=radiant_team_ids, dire_team_ids=dire_team_ids, winning_team="radiant"
        )

        # Verify results
        # Radiant players should have wins
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Radiant player {player.name} should have 1 win"
            assert player.losses == 0, f"Radiant player {player.name} should have 0 losses"

        # Dire players should have losses
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Dire player {player.name} should have 0 wins"
            assert player.losses == 1, f"Dire player {player.name} should have 1 loss"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
