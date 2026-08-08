"""
Consolidated tests for match recording.

Merges the previously fragmented test_match_recording_* files into one module:
basic win/loss recording, radiant/dire mapping, betting integration, admin
override + voting, and bug regressions.
"""

import json

import pytest

from config import JOPACOIN_PER_GAME, JOPACOIN_WIN_REWARD
from repositories.bet_repository import BetRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID
from tests.repository_harness import RepositoryTestDatabase as Database

# =============================================================================
# SHARED FIXTURES
# =============================================================================


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


def test_match_easter_egg_batches_personal_streak_records(
    player_repository, match_repository, monkeypatch
):
    player_ids = [99101, 99102]
    _seed_repo_players(player_repository, player_ids)
    player_repository.update_personal_best_win_streak(
        player_ids[0], TEST_GUILD_ID, 4
    )
    player_repository.update_personal_best_win_streak(
        player_ids[1], TEST_GUILD_ID, 6
    )
    service = MatchService(
        player_repo=player_repository,
        match_repo=match_repository,
    )

    connection_count = 0
    original_get_connection = player_repository.get_connection

    def counted_get_connection():
        nonlocal connection_count
        connection_count += 1
        return original_get_connection()

    monkeypatch.setattr(player_repository, "get_connection", counted_get_connection)

    easter_eggs = service._collect_match_easter_eggs(
        radiant_team_ids=player_ids,
        dire_team_ids=[],
        winning_ids=player_ids,
        expected_ids=set(player_ids),
        streak_data={
            player_ids[0]: (5, 1.0),
            player_ids[1]: (6, 1.0),
        },
        guild_id=TEST_GUILD_ID,
    )

    assert easter_eggs["win_streak_records"] == [
        {
            "discord_id": player_ids[0],
            "current_streak": 5,
            "previous_best": 4,
        }
    ]
    # One milestone-player read plus one atomic personal-best batch.
    assert connection_count == 2


def test_match_easter_egg_batches_rivalries_with_correct_perspective(
    player_repository, match_repository, pairings_repository, monkeypatch
):
    dire_id = 99200
    radiant_ids = [99301, 99302]
    _seed_repo_players(player_repository, [dire_id, *radiant_ids])
    for match_id in range(1, 11):
        pairings_repository.update_pairings_for_match(
            match_id=match_id,
            guild_id=TEST_GUILD_ID,
            team1_ids=radiant_ids,
            team2_ids=[dire_id],
            winning_team=1 if match_id <= 8 else 2,
        )

    service = MatchService(
        player_repo=player_repository,
        match_repo=match_repository,
        pairings_repo=pairings_repository,
    )
    connection_count = 0
    original_get_connection = pairings_repository.get_connection

    def counted_get_connection():
        nonlocal connection_count
        connection_count += 1
        return original_get_connection()

    monkeypatch.setattr(
        pairings_repository, "get_connection", counted_get_connection
    )

    easter_eggs = service._collect_match_easter_eggs(
        radiant_team_ids=radiant_ids,
        dire_team_ids=[dire_id],
        winning_ids=radiant_ids,
        expected_ids={dire_id, *radiant_ids},
        streak_data={},
        guild_id=TEST_GUILD_ID,
    )

    assert easter_eggs["rivalries_detected"] == [
        {
            "player1_id": radiant_ids[0],
            "player2_id": dire_id,
            "games_against": 10,
            "winrate_vs": 80.0,
        },
        {
            "player1_id": radiant_ids[1],
            "player2_id": dire_id,
            "games_against": 10,
            "winrate_vs": 80.0,
        },
    ]
    # The two opponent histories and the teammate history use one bulk read.
    assert connection_count == 1


# =============================================================================
# === Win/Loss API ===
# =============================================================================
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

    def test_radiant_dire_team_mapping(self, test_db):
        """Round-trip test: radiant/dire win results are persisted correctly.

        Records one match where Radiant wins and one where Dire wins, then
        verifies the persisted match_participants rows mark the correct side
        as winners in each case.
        """
        player_ids = list(range(3001, 3011))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        radiant = player_ids[:5]
        dire = player_ids[5:]

        def fetch_participants(match_id):
            conn = test_db.get_connection()
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id, side, won FROM match_participants WHERE match_id = ?",
                (match_id,),
            )
            rows = cursor.fetchall()
            conn.close()
            return rows

        # Match 1: Radiant wins
        match_id = test_db.record_match(
            radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant"
        )
        rows = fetch_participants(match_id)
        assert len(rows) == 10
        for row in rows:
            if row["discord_id"] in radiant:
                assert row["side"] == "radiant"
                assert row["won"] == 1, f"Radiant player {row['discord_id']} should be a winner"
            else:
                assert row["side"] == "dire"
                assert row["won"] == 0, f"Dire player {row['discord_id']} should be a loser"

        # Match 2: Dire wins - the inverse must hold
        match_id = test_db.record_match(
            radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire"
        )
        rows = fetch_participants(match_id)
        assert len(rows) == 10
        for row in rows:
            if row["discord_id"] in radiant:
                assert row["side"] == "radiant"
                assert row["won"] == 0, f"Radiant player {row['discord_id']} should be a loser"
            else:
                assert row["side"] == "dire"
                assert row["won"] == 1, f"Dire player {row['discord_id']} should be a winner"

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)


class TestRadiantDireBugFixRecording:
    """Regression tests for the bug where Dire wins were recorded as losses.

    These drive the real mapping owned by ``MatchService.record_match``:
    winners/losers are derived from the ``winning_team`` side name, radiant is
    always stored as team1 / dire as team2 in the matches row, and the stored
    ``winning_team`` column is 1 for a radiant win, 2 for a dire win.
    """

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    # Exact team composition from the original bug report.
    RADIANT_NAMES = [
        "FakeUser917762",
        "FakeUser924119",
        "FakeUser926408",
        "FakeUser921765",
        "FakeUser925589",
    ]
    DIRE_NAMES = [
        "FakeUser923487",
        "BugReporter",  # The user who reported the bug
        "FakeUser921510",
        "FakeUser920053",
        "FakeUser919197",
    ]

    def _setup_bug_report_match(self, test_db, base_id):
        """Seed the 10 bug-report players and pin the exact teams on the
        real pending match state (mutate + persist, the established pattern)."""
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=True,
        )

        all_ids = list(range(base_id, base_id + 10))
        for pid, name in zip(all_ids, self.RADIANT_NAMES + self.DIRE_NAMES):
            player_repo.add(
                discord_id=pid,
                discord_username=name,
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        radiant_team_ids = all_ids[:5]
        dire_team_ids = all_ids[5:]

        match_service.shuffle_players(all_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        pending.radiant_team_ids = radiant_team_ids
        pending.dire_team_ids = dire_team_ids
        match_service._persist_match_state(TEST_GUILD_ID, pending)

        return match_service, player_repo, radiant_team_ids, dire_team_ids

    @staticmethod
    def _fetch_match_row(test_db, match_id):
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT team1_players, team2_players, winning_team FROM matches WHERE match_id = ?",
            (match_id,),
        )
        row = cursor.fetchone()
        conn.close()
        return row

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
        match_service, player_repo, radiant_team_ids, dire_team_ids = (
            self._setup_bug_report_match(test_db, base_id=90001)
        )

        # Drive the REAL production mapping: record "dire" won.
        result = match_service.record_match("dire", guild_id=TEST_GUILD_ID)

        assert sorted(result["winning_player_ids"]) == sorted(dire_team_ids)
        assert sorted(result["losing_player_ids"]) == sorted(radiant_team_ids)

        # CRITICAL TEST: Verify Dire players (who won) have WINS
        for pid in dire_team_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1, (
                f"BUG REPRODUCTION: Dire player {player.name} (ID: {pid}) should have 1 win, got {player.wins}"
            )
            assert player.losses == 0, (
                f"BUG REPRODUCTION: Dire player {player.name} (ID: {pid}) should have 0 losses, got {player.losses}"
            )

        # CRITICAL TEST: Verify Radiant players (who lost) have LOSSES
        for pid in radiant_team_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0, (
                f"BUG REPRODUCTION: Radiant player {player.name} (ID: {pid}) should have 0 wins, got {player.wins}"
            )
            assert player.losses == 1, (
                f"BUG REPRODUCTION: Radiant player {player.name} (ID: {pid}) should have 1 loss, got {player.losses}"
            )

        # Pin the persisted matches row: radiant stored as team1, dire as
        # team2, and a dire win stored as winning_team=2.
        row = self._fetch_match_row(test_db, result["match_id"])
        assert json.loads(row["team1_players"]) == radiant_team_ids
        assert json.loads(row["team2_players"]) == dire_team_ids
        assert row["winning_team"] == 2

        # Specifically verify BugReporter (the user who reported the bug) has a WIN
        reporter_player = next(
            player_repo.get_by_id(pid, TEST_GUILD_ID)
            for pid in dire_team_ids
            if player_repo.get_by_id(pid, TEST_GUILD_ID).name == "BugReporter"
        )
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
        match_service, player_repo, radiant_team_ids, dire_team_ids = (
            self._setup_bug_report_match(test_db, base_id=91001)
        )

        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert sorted(result["winning_player_ids"]) == sorted(radiant_team_ids)
        assert sorted(result["losing_player_ids"]) == sorted(dire_team_ids)

        # Verify Radiant players (who won) have WINS
        for pid in radiant_team_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1, (
                f"Radiant player {player.name} should have 1 win, got {player.wins}"
            )
            assert player.losses == 0, (
                f"Radiant player {player.name} should have 0 losses, got {player.losses}"
            )

        # Verify Dire players (who lost) have LOSSES
        for pid in dire_team_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0, (
                f"Dire player {player.name} should have 0 wins, got {player.wins}"
            )
            assert player.losses == 1, (
                f"Dire player {player.name} should have 1 loss, got {player.losses}"
            )

        # A radiant win is stored as winning_team=1 with radiant as team1.
        row = self._fetch_match_row(test_db, result["match_id"])
        assert json.loads(row["team1_players"]) == radiant_team_ids
        assert json.loads(row["team2_players"]) == dire_team_ids
        assert row["winning_team"] == 1

    def test_team_number_validation(self, test_db):
        """record_match rejects anything but 'radiant'/'dire', hands out no
        stats on the failed attempt, and stays recordable afterwards."""
        match_service, player_repo, radiant_team_ids, dire_team_ids = (
            self._setup_bug_report_match(test_db, base_id=92001)
        )

        with pytest.raises(ValueError, match="winning_team must be 'radiant' or 'dire'"):
            match_service.record_match("team1", guild_id=TEST_GUILD_ID)

        # Nothing was recorded by the rejected attempt.
        for pid in radiant_team_ids + dire_team_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0, f"Player {pid} should have 0 wins after rejected record"
            assert player.losses == 0, f"Player {pid} should have 0 losses after rejected record"

        # The failed attempt must not wedge recording: a valid retry succeeds.
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        assert sorted(result["winning_player_ids"]) == sorted(radiant_team_ids)

        # Once recorded, the pending state is cleared and a re-record fails.
        with pytest.raises(ValueError, match="No recent shuffle found"):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)


class TestGlickoRatingPersistence:
    """Verify Glicko-2 ratings are actually persisted after record_match.

    The rating math is unit-tested elsewhere; this guards against a dropped
    write between MatchService.record_match and the players table.
    """

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_glicko_ratings_persisted_after_record_match(self, test_db):
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=True,
        )

        player_ids = list(range(7001, 7011))
        _seed_repo_players(player_repo, player_ids)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        winners = list(pending.radiant_team_ids)
        losers = list(pending.dire_team_ids)

        ratings_before = {
            pid: player_repo.get_by_id(pid, TEST_GUILD_ID).glicko_rating
            for pid in player_ids
        }
        assert all(rating == 1500.0 for rating in ratings_before.values())

        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]

        # Winners' stored ratings increased; losers' decreased.
        for pid in winners:
            after = player_repo.get_by_id(pid, TEST_GUILD_ID).glicko_rating
            assert after > ratings_before[pid], (
                f"Winner {pid} glicko_rating should increase: {ratings_before[pid]} -> {after}"
            )
        for pid in losers:
            after = player_repo.get_by_id(pid, TEST_GUILD_ID).glicko_rating
            assert after < ratings_before[pid], (
                f"Loser {pid} glicko_rating should decrease: {ratings_before[pid]} -> {after}"
            )

        # rating_history rows exist for all 10 participants and agree on won flag.
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT discord_id, rating, rating_before, won FROM rating_history WHERE match_id = ?",
            (match_id,),
        )
        rows = cursor.fetchall()
        conn.close()

        assert len(rows) == 10
        for row in rows:
            assert row["rating_before"] == 1500.0
            if row["discord_id"] in winners:
                assert row["won"] == 1
                assert row["rating"] > row["rating_before"]
            else:
                assert row["won"] == 0
                assert row["rating"] < row["rating_before"]


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
        participant = pending.radiant_team_ids[0]
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
        assert result["jc_changes"][participant] == {"payout": 10, "bet": 5}
        assert result["jc_changes"][spectator] == {"bet": -5}

        expected_participant_balance = 3 + 20 - 5 + 10 + JOPACOIN_WIN_REWARD
        assert player_repo.get_balance(participant, TEST_GUILD_ID) == expected_participant_balance
        # Spectator starts with 3, gets +10 top-up, -5 lost bet = 8
        assert player_repo.get_balance(spectator, TEST_GUILD_ID) == 8

    def test_lobby_rewards_pay_ten_to_winners_and_five_to_others(self, test_db):
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
        excluded_ids = pending.excluded_player_ids
        assert len(excluded_ids) == 2

        # Start everyone at zero for deterministic balance checks
        for pid in player_ids:
            player_repo.update_balance(pid, TEST_GUILD_ID, 0)

        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        for pid in excluded_ids:
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == 5
            assert result["jc_changes"][pid] == {"payout": 5}

        # Included players got the win bonus (radiant won) or participation
        # (dire lost). Pin each player's exact reward by the side they landed
        # on so the assertion catches a regression that pays winners the loser
        # reward or vice versa (which the prior {win, lose} membership check
        # silently accepted).
        radiant_ids = set(pending.radiant_team_ids)
        included_ids = set(player_ids) - set(excluded_ids)
        for pid in included_ids:
            expected = 10 if pid in radiant_ids else 5
            assert player_repo.get_balance(pid, TEST_GUILD_ID) == expected
            assert result["jc_changes"][pid] == {"payout": expected}

    def test_readycheck_nonresponders_receive_no_match_jc(self, test_db):
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

        player_ids = list(range(5151, 5161))
        nonresponder = 5161
        for pid in [*player_ids, nonresponder]:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_repo.update_balance(pid, TEST_GUILD_ID, 0)

        match_service.shuffle_players(
            player_ids,
            guild_id=TEST_GUILD_ID,
            excluded_conditional_ids=[nonresponder],
        )
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert player_repo.get_balance(nonresponder, TEST_GUILD_ID) == 0
        assert nonresponder not in result["jc_changes"]

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
        assert pending.excluded_player_ids == excluded_ids

        # Verify radiant + dire + excluded = all players
        all_in_match = set(pending.radiant_team_ids + pending.dire_team_ids)
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

        Also covers multi-bettor aggregation per side on one match (merges
        test_betting_totals_multiple_bets_same_match): match 1 carries two
        bets per side and the totals must sum them.
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

        spectator4 = 9004
        spectator5 = 9005
        for extra in (spectator4, spectator5):
            player_repo.add(
                discord_id=extra,
                discord_username=f"Spectator{extra}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1100,
                glicko_rating=1100.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        for spec in (spectator1, spectator2, spectator4, spectator5):
            player_repo.add_balance(spec, TEST_GUILD_ID, 20)

        # Place two bets per side on the first match: totals must aggregate.
        betting_service.place_bet(TEST_GUILD_ID, spectator1, "radiant", 3, pending1)
        betting_service.place_bet(TEST_GUILD_ID, spectator4, "radiant", 5, pending1)
        betting_service.place_bet(TEST_GUILD_ID, spectator2, "dire", 2, pending1)
        betting_service.place_bet(TEST_GUILD_ID, spectator5, "dire", 4, pending1)

        # Verify totals sum the pending bets per side
        totals = betting_service.get_pot_odds(TEST_GUILD_ID, pending_state=pending1)
        assert totals["radiant"] == 8, "Should show 8 jopacoin on Radiant (3+5)"
        assert totals["dire"] == 6, "Should show 6 jopacoin on Dire (2+4)"

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
        bet = bet_repo.get_player_pending_bet(TEST_GUILD_ID, spectator3, since_ts=pending2.shuffle_timestamp)
        assert bet is not None, "Bet should exist"
        assert bet["amount"] == 6, "Bet amount should be 6"
        assert bet["team_bet_on"] == "dire", "Bet should be on Dire"

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
        """Loan is repaid when borrower participates in a match (winning team).

        Also pins the repayment report in the record result and the deferred
        fee flowing to the nonprofit fund (merges
        test_loan_repayment_result_in_match_record and
        test_loan_repayment_fee_goes_to_nonprofit).
        """
        player_repo = services["player_repo"]
        loan_service = services["loan_service"]
        loan_repo = services["loan_repo"]
        match_service = services["match_service"]

        # Borrower starts with 0 balance (after default 3)
        borrower_id = test_players[0]
        player_repo.update_balance(borrower_id, TEST_GUILD_ID, 0)
        nonprofit_before = loan_repo.get_nonprofit_fund(TEST_GUILD_ID)

        # Take a loan of 50 (fee = 10, total owed = 60)
        result = loan_service.execute_loan(borrower_id, 50, guild_id=TEST_GUILD_ID)
        assert result.success
        assert player_repo.get_balance(borrower_id, TEST_GUILD_ID) == 50  # Got full loan amount

        # Verify outstanding loan exists; the fee is deferred, not yet funded
        state = loan_service.get_state(borrower_id, guild_id=TEST_GUILD_ID)
        assert state.has_outstanding_loan
        assert state.outstanding_principal == 50
        assert state.outstanding_fee == 10
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == nonprofit_before

        # Shuffle and record match (borrower is on radiant, radiant wins)
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)

        # Make sure borrower is on radiant for predictable test
        if borrower_id not in pending.radiant_team_ids:
            # Swap teams so borrower is on winning side
            pending.radiant_team_ids, pending.dire_team_ids = (
                pending.dire_team_ids,
                pending.radiant_team_ids,
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

        # The repayment is reported in the record result
        repayments = result["loan_repayments"]
        assert len(repayments) == 1
        assert repayments[0]["player_id"] == borrower_id
        assert repayments[0]["principal"] == 50
        assert repayments[0]["fee"] == 10
        assert repayments[0]["total_repaid"] == 60

        # The deferred fee has now reached the nonprofit fund
        assert loan_repo.get_nonprofit_fund(TEST_GUILD_ID) == nonprofit_before + 10

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
        if borrower_id not in pending.dire_team_ids:
            pending.radiant_team_ids, pending.dire_team_ids = (
                pending.dire_team_ids,
                pending.radiant_team_ids,
            )
            # Persist the swapped state
            match_service._persist_match_state(TEST_GUILD_ID, pending)

        # Radiant wins (borrower loses)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Loan still repaid
        state = loan_service.get_state(borrower_id, guild_id=TEST_GUILD_ID)
        assert not state.has_outstanding_loan

        # Balance: 0 + 50 loan - 60 repayment + JOPACOIN_PER_GAME participation
        assert player_repo.get_balance(borrower_id, TEST_GUILD_ID) == -10 + JOPACOIN_PER_GAME

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
        pending2 = match_service.get_last_shuffle(TEST_GUILD_ID)
        spectator_won = spectator_id in pending2.radiant_team_ids  # radiant wins below
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Loan repaid now
        state = loan_service.get_state(spectator_id, guild_id=TEST_GUILD_ID)
        assert not state.has_outstanding_loan

        # Balance: 130 - 120 repayment + the exact match reward for the side the
        # spectator actually landed on (win reward if on radiant, else participation).
        # Pinning the branch catches a regression that pays winners the loser
        # reward (or vice versa).
        expected_reward = JOPACOIN_WIN_REWARD if spectator_won else JOPACOIN_PER_GAME
        balance = player_repo.get_balance(spectator_id, TEST_GUILD_ID)
        assert balance == 10 + expected_reward

    def test_multiple_players_with_loans(self, services, test_players):
        """Multiple players have loans, all repaid after one match — including
        a borrower who already spent the loan money and is pushed into debt by
        the repayment (merges test_loan_repayment_pushes_into_debt)."""
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
        # Borrower2 "spends" the loan money before playing.
        player_repo.update_balance(borrower2, TEST_GUILD_ID, 0)

        # Shuffle and record
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        b1_won = borrower1 in pending.radiant_team_ids  # radiant wins below
        b2_won = borrower2 in pending.radiant_team_ids
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Both loans repaid
        assert not loan_service.get_state(borrower1, guild_id=TEST_GUILD_ID).has_outstanding_loan
        assert not loan_service.get_state(borrower2, guild_id=TEST_GUILD_ID).has_outstanding_loan

        # Balances: loan - repayment + the exact reward for each borrower's side.
        # Pinning each branch catches a regression that swaps win/participation
        # rewards. Borrower2 spent the loan, so the repayment leaves them in
        # deep debt (-120 + reward).
        b1_reward = JOPACOIN_WIN_REWARD if b1_won else JOPACOIN_PER_GAME
        b2_reward = JOPACOIN_WIN_REWARD if b2_won else JOPACOIN_PER_GAME
        b1_balance = player_repo.get_balance(borrower1, TEST_GUILD_ID)
        b2_balance = player_repo.get_balance(borrower2, TEST_GUILD_ID)

        assert b1_balance == 50 - 60 + b1_reward
        assert b2_balance == -120 + b2_reward


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

    def test_has_admin_submission_transitions(self, admin_match_service, admin_test_players):
        """has_admin_submission is False with no/only-non-admin submissions and
        flips True (unlocking can_record_match) once an admin submits.

        Merges test_has_admin_submission_with_no_submissions,
        _with_non_admin_submission, _with_admin_submission and
        test_can_record_match_with_admin_override.
        """
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=5001, result="radiant", is_admin=False)
        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)
        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is True
        # Admin bypasses the 3-non-admin requirement (only 1 non-admin voted).
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 1

    def test_admin_override_allows_immediate_recording(self, admin_match_service, admin_test_players):
        """An admin submission alongside insufficient non-admin votes readies
        the record immediately, the match records, and all pending state is
        cleared afterwards.

        Merges test_admin_override_with_mixed_submissions and
        test_admin_override_clears_state_after_recording.
        """
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Add 1 non-admin submission (not enough on its own)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=5001, result="radiant", is_admin=False)
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False

        # Admin submits - should override and allow immediate recording
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True
        )

        assert submission["is_ready"] is True
        assert submission["non_admin_count"] == 1  # Still only 1 non-admin
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True

        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert record_result["match_id"] is not None
        assert record_result["winning_team"] == "radiant"
        assert record_result["updated_count"] == 10

        # State should be cleared after recording
        assert admin_match_service.get_last_shuffle(TEST_GUILD_ID) is None
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False


class TestFirstToThreeVoting:
    """Test first-to-3 voting system for non-admin match recording."""

    def test_get_vote_counts_tracks_votes(self, admin_match_service, voting_test_players):
        """get_vote_counts starts at zero, excludes admin votes, tracks
        conflicting non-admin votes, and add_record_submission returns the
        running counts.

        Merges test_get_vote_counts_empty, test_get_vote_counts_excludes_admin,
        test_conflicting_votes_allowed and test_submission_returns_vote_counts.
        """
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        assert admin_match_service.get_vote_counts(TEST_GUILD_ID) == {"radiant": 0, "dire": 0}

        # Admin votes are never counted.
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)
        assert admin_match_service.get_vote_counts(TEST_GUILD_ID) == {"radiant": 0, "dire": 0}

        # Conflicting non-admin votes are allowed and tracked; the submission
        # result reports the running counts.
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6001, result="radiant", is_admin=False)
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=6002, result="dire", is_admin=False
        )
        assert submission["vote_counts"] == {"radiant": 1, "dire": 1}

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6003, result="radiant", is_admin=False)
        assert admin_match_service.get_vote_counts(TEST_GUILD_ID) == {"radiant": 2, "dire": 1}

    def test_first_to_3_radiant_wins(self, admin_match_service, voting_test_players):
        """Radiant wins when it reaches 3 votes first; fewer than 3 votes for
        one side is never ready (merges the non-admin submission-count
        assertions of test_can_record_match_without_admin_requires_3_non_admin).
        """
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # 2 radiant, 2 dire - not ready
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6003, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6004, result="dire", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) is None
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 4

        # 3rd radiant vote - radiant wins!
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=6005, result="radiant", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) == "radiant"
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 5

    def test_first_to_3_dire_wins(self, admin_match_service, voting_test_players):
        """Test that dire wins when it reaches 3 votes first."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # 1 radiant, 2 dire
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6003, result="dire", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False

        # 3rd dire vote - dire wins!
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=6004, result="dire", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "dire"
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) == "dire"

    def test_user_cannot_change_vote(self, admin_match_service, voting_test_players):
        """A user cannot change their vote, but re-submitting the same vote is
        an accepted no-op (merges test_user_can_revote_same_result)."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6001, result="radiant", is_admin=False)

        # Same user tries to vote differently
        with pytest.raises(ValueError, match="already submitted"):
            admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6001, result="dire", is_admin=False)

        # Same vote again - should not raise, and not double-count
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6001, result="radiant", is_admin=False)
        assert admin_match_service.get_vote_counts(TEST_GUILD_ID) == {"radiant": 1, "dire": 0}

    def test_first_to_3_records_correct_winner(self, admin_match_service, voting_test_players):
        """Test that the match is recorded with the correct winner."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Radiant gets 3 votes, Dire gets 2
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6001, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6002, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6003, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=6004, result="radiant", is_admin=False)
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=6005, result="radiant", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"

        # Record the match
        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert record_result["winning_team"] == "radiant"
        assert record_result["match_id"] is not None


class TestAbortVoting:
    """Test abort submission handling for match recording."""

    def test_non_admin_abort_requires_four_lobby_votes(self, admin_match_service, abort_test_players):
        """Four lobby votes are required to abort, non-lobby players cannot
        vote, and clearing the shuffle resets the abort state.

        Merges test_non_lobby_player_cannot_vote_to_abort and
        test_clear_abort_state_after_abort.
        """
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False

        # Someone outside the shuffled lobby cannot vote at all.
        with pytest.raises(ValueError, match="shuffled lobby"):
            admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=9999, is_admin=False)
        assert admin_match_service.get_abort_submission_count(TEST_GUILD_ID) == 0

        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=7001, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=7002, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=7003, is_admin=False)

        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False
        submission = admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=7004, is_admin=False)
        assert submission["is_ready"] is True
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True

        # Clearing the shuffle (the abort itself) resets the abort state.
        admin_match_service.clear_last_shuffle(TEST_GUILD_ID)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False

    def test_admin_abort_overrides_minimum(self, admin_match_service, abort_test_players):
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        submission = admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=9999, is_admin=True)

        assert submission["is_ready"] is True
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True
        assert submission["non_admin_count"] == admin_match_service.get_abort_submission_count(TEST_GUILD_ID)


# =============================================================================
# === Bug regressions ===
# =============================================================================


class TestExcludedPlayersBug:
    """Test the critical bug where excluded players were getting losses recorded."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @staticmethod
    def _make_service(test_db):
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=True,
        )
        return match_service, player_repo

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
        match_service, player_repo = self._make_service(test_db)

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
            player_repo.add(
                discord_id=discord_id,
                discord_username=name,
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Real shuffle with 11 players, then pin BugReporter as the excluded
        # player and the other ten onto the teams (mutate + persist pattern).
        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        pending = match_service.get_last_shuffle(TEST_GUILD_ID)
        radiant_team_ids = player_ids[:5]
        dire_team_ids = player_ids[5:10]
        excluded_player_id = player_ids[10]  # BugReporter
        pending.radiant_team_ids = radiant_team_ids
        pending.dire_team_ids = dire_team_ids
        pending.excluded_player_ids = [excluded_player_id]
        match_service._persist_match_state(TEST_GUILD_ID, pending)

        # Record match through the REAL production path - Dire won
        result = match_service.record_match("dire", guild_id=TEST_GUILD_ID)
        match_id = result["match_id"]
        assert result["excluded_player_ids"] == [excluded_player_id]

        # CRITICAL TEST: Excluded player should have 0 wins, 0 losses
        excluded_player = player_repo.get_by_id(excluded_player_id, TEST_GUILD_ID)
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
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 1, f"Dire player {player.name} should have 1 win"
            assert player.losses == 0, f"Dire player {player.name} should have 0 losses"

        for pid in radiant_team_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0, f"Radiant player {player.name} should have 0 wins"
            assert player.losses == 1, f"Radiant player {player.name} should have 1 loss"

        # Pin the DB rows: the excluded player is in neither stored team and
        # got no rating_history entry for this match.
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT team1_players, team2_players FROM matches WHERE match_id = ?",
            (match_id,),
        )
        row = cursor.fetchone()
        cursor.execute(
            "SELECT COUNT(*) AS n FROM rating_history WHERE match_id = ? AND discord_id = ?",
            (match_id, excluded_player_id),
        )
        history_count = cursor.fetchone()["n"]
        conn.close()

        stored_ids = set(json.loads(row["team1_players"])) | set(json.loads(row["team2_players"]))
        assert excluded_player_id not in stored_ids, (
            f"BUG: Excluded player {excluded_player_id} found in stored match teams!"
        )
        assert stored_ids == set(radiant_team_ids + dire_team_ids)
        assert history_count == 0, "Excluded player must not get a rating_history row"

    def test_excluded_players_validation(self, test_db):
        """Production refuses to record a match whose teams contain an
        excluded player, and hands out no stats on the rejected attempt."""
        match_service, player_repo = self._make_service(test_db)

        # Create 11 players
        player_ids = list(range(97001, 97012))
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
        # Corrupt the state: mark a player who is also on radiant as excluded.
        pending.radiant_team_ids = player_ids[:5]
        pending.dire_team_ids = player_ids[5:10]
        pending.excluded_player_ids = [player_ids[0], player_ids[10]]
        match_service._persist_match_state(TEST_GUILD_ID, pending)

        with pytest.raises(ValueError, match="Excluded players detected in match teams"):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # No match row was created and nobody got a win or loss.
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT COUNT(*) AS n FROM matches")
        match_count = cursor.fetchone()["n"]
        conn.close()
        assert match_count == 0

        for pid in player_ids:
            player = player_repo.get_by_id(pid, TEST_GUILD_ID)
            assert player.wins == 0
            assert player.losses == 0


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
