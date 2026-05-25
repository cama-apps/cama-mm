"""Match-recording edge cases: bad arguments, team-size handling, invariants.

Two layers are exercised:

* ``MatchService.record_match`` — the production path. It reads the persisted
  shuffle state, so it cannot be handed arbitrary teams; the edge it *does*
  guard is the excluded-player invariant (a player both excluded and on a team
  must abort the recording) and the winning_team enum.
* ``MatchRepository.record_match`` / ``Database.record_match`` — the storage
  primitive. This is where empty / mismatched team lists are accepted or
  rejected.

NOTE (surfaced gap): ``MatchRepository.record_match`` performs **no team-size
validation**. A 3-vs-7 split, or an empty Radiant list, is inserted silently —
``match_participants`` simply ends up lopsided. The repo only validates the
*shape* of the call (None team lists, bad ``winning_team``). The tests below
pin the *actual current behavior* rather than an aspirational error so they
stay green; ``test_mismatched_team_sizes_currently_not_rejected`` is written to
fail loudly if size validation is later added, prompting an update + a real
error-path assertion. Adding that validation is a source change outside this
test module's scope.
"""

import json

import pytest

from database import Database
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


def _seed(player_repo, ids, guild_id=TEST_GUILD_ID):
    for pid in ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=guild_id,
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
            preferred_roles=["1", "2", "3", "4", "5"],
        )


# =============================================================================
# MatchService.record_match — argument + invariant checks
# =============================================================================


class TestRecordMatchServiceGuards:
    """The production record path rejects bad winning_team and missing shuffle."""

    @pytest.fixture
    def match_service(self, repo_db_path):
        player_repo = PlayerRepository(repo_db_path)
        match_repo = MatchRepository(repo_db_path)
        return MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    def test_record_without_shuffle_raises(self, match_service):
        """No pending shuffle -> clear ValueError, not a silent no-op."""
        with pytest.raises(ValueError, match="No recent shuffle found"):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    def test_record_with_invalid_winning_team_raises(self, match_service, repo_db_path):
        """winning_team outside {'radiant','dire'} must be rejected after shuffle."""
        player_repo = PlayerRepository(repo_db_path)
        player_ids = list(range(40000, 40010))
        _seed(player_repo, player_ids)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        with pytest.raises(ValueError, match="winning_team must be"):
            match_service.record_match("spectator", guild_id=TEST_GUILD_ID)

        # The shuffle must remain pending — a rejected record consumes nothing.
        assert match_service.get_last_shuffle(TEST_GUILD_ID) is not None

    def test_excluded_player_on_a_team_aborts_recording(self, match_service, repo_db_path):
        """The excluded-player invariant: a player both excluded and rostered
        must abort recording rather than be double-counted.

        We drive a real >10-player pool shuffle (which produces genuine
        exclusions), then corrupt the persisted state so an excluded id also
        appears on Radiant — exactly the inconsistency the guard exists for.
        """
        player_repo = PlayerRepository(repo_db_path)
        player_ids = list(range(41000, 41012))  # 12 players -> 2 excluded
        _seed(player_repo, player_ids)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert state.excluded_player_ids, "12-player pool shuffle should exclude 2"

        # Inject an excluded id onto Radiant to simulate corrupt state.
        bad_id = state.excluded_player_ids[0]
        state.radiant_team_ids = [bad_id] + state.radiant_team_ids[1:]
        match_service.match_repo.update_pending_match(
            state.pending_match_id,
            match_service._build_pending_match_payload(state),
            guild_id=TEST_GUILD_ID,
        )

        with pytest.raises(ValueError, match="Excluded players detected"):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    def test_clean_shuffle_excluded_set_disjoint_from_teams(self, match_service, repo_db_path):
        """Sanity invariant: a normal pool shuffle never overlaps excluded with teams."""
        player_repo = PlayerRepository(repo_db_path)
        player_ids = list(range(42000, 42014))  # 14 players -> 4 excluded
        _seed(player_repo, player_ids)

        match_service.shuffle_players(player_ids, guild_id=TEST_GUILD_ID)
        state = match_service.get_last_shuffle(TEST_GUILD_ID)

        rostered = set(state.radiant_team_ids) | set(state.dire_team_ids)
        excluded = set(state.excluded_player_ids)
        assert len(rostered) == 10
        assert len(excluded) == 4
        assert rostered.isdisjoint(excluded), "Excluded players must not be rostered"
        # Every input player is accounted for exactly once.
        assert rostered | excluded == set(player_ids)


# =============================================================================
# MatchRepository.record_match — storage primitive edge cases
# =============================================================================


class TestRecordMatchRepositoryArgChecks:
    """The repo validates call *shape* even though it skips size validation."""

    @pytest.fixture
    def db(self, repo_db_path):
        database = Database(repo_db_path)
        for pid in range(43000, 43010):
            database.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return database

    def test_new_api_requires_both_team_lists(self, db):
        """Passing only one side with the string API is a clear error."""
        with pytest.raises(ValueError, match="New API requires"):
            db.record_match(radiant_team_ids=[43000, 43001], winning_team="radiant")

    def test_new_api_rejects_unknown_winning_team(self, db):
        with pytest.raises(ValueError, match="winning_team must be 'radiant' or 'dire'"):
            db.record_match(
                radiant_team_ids=[43000, 43001, 43002, 43003, 43004],
                dire_team_ids=[43005, 43006, 43007, 43008, 43009],
                winning_team="purple",
            )

    def test_old_api_requires_both_team_lists(self, db):
        """Old int-winning_team API also rejects a missing side."""
        with pytest.raises(ValueError, match="Old API requires"):
            db.record_match(winning_team=1)

    def test_mismatched_team_sizes_currently_not_rejected(self, db):
        """SURFACED GAP: a 3-vs-7 split is accepted with no error.

        This pins current behavior. ``record_match`` writes the match row and
        all 10 ``match_participants`` rows on the lopsided split. If team-size
        validation is added later, this test will fail — and SHOULD be replaced
        with a ``pytest.raises`` assertion on the new error.
        """
        match_id = db.record_match(
            radiant_team_ids=[43000, 43001, 43002],
            dire_team_ids=[43003, 43004, 43005, 43006, 43007, 43008, 43009],
            winning_team="radiant",
        )
        assert match_id > 0  # No error today.

        conn = db.get_connection()
        try:
            radiant = conn.execute(
                "SELECT COUNT(*) AS c FROM match_participants "
                "WHERE match_id = ? AND side = 'radiant'",
                (match_id,),
            ).fetchone()["c"]
            dire = conn.execute(
                "SELECT COUNT(*) AS c FROM match_participants "
                "WHERE match_id = ? AND side = 'dire'",
                (match_id,),
            ).fetchone()["c"]
        finally:
            conn.close()
        # The lopsided split is stored verbatim — documents the missing guard.
        assert (radiant, dire) == (3, 7), (
            "If this fails, record_match likely gained size validation — "
            "replace this test with a pytest.raises on the new error."
        )

    def test_empty_team_currently_not_rejected(self, db):
        """SURFACED GAP: an empty Radiant list is accepted with no error."""
        match_id = db.record_match(
            radiant_team_ids=[],
            dire_team_ids=[43000, 43001, 43002, 43003, 43004],
            winning_team="dire",
        )
        assert match_id > 0  # No error today.

        conn = db.get_connection()
        try:
            row = conn.execute(
                "SELECT team1_players, team2_players FROM matches WHERE match_id = ?",
                (match_id,),
            ).fetchone()
        finally:
            conn.close()
        assert json.loads(row["team1_players"]) == []
        assert json.loads(row["team2_players"]) == [43000, 43001, 43002, 43003, 43004]


class TestRecordMatchRepositoryParticipants:
    """A well-formed call records balanced participants with correct sides/wins."""

    @pytest.fixture
    def repo(self, repo_db_path):
        return MatchRepository(repo_db_path)

    def test_balanced_record_writes_side_and_won_flags(self, repo):
        radiant = [44000, 44001, 44002, 44003, 44004]
        dire = [44005, 44006, 44007, 44008, 44009]

        match_id = repo.record_match(
            team1_ids=radiant,
            team2_ids=dire,
            winning_team=1,  # Radiant won
            guild_id=TEST_GUILD_ID,
        )
        assert match_id > 0

        participants = repo.get_match_participants(match_id, guild_id=TEST_GUILD_ID)
        by_id = {p["discord_id"]: p for p in participants}
        assert len(by_id) == 10, "All 10 players recorded exactly once"

        for pid in radiant:
            assert by_id[pid]["side"] == "radiant"
            assert by_id[pid]["team_number"] == 1
            assert by_id[pid]["won"] == 1
        for pid in dire:
            assert by_id[pid]["side"] == "dire"
            assert by_id[pid]["team_number"] == 2
            assert by_id[pid]["won"] == 0

    def test_duplicate_player_across_teams_violates_participant_pk(self, repo):
        """A player listed on both teams is rejected by the participant PK.

        ``match_participants`` has PRIMARY KEY (match_id, discord_id), so the
        second insert of the same id (on the other side) raises IntegrityError.
        Duplicate-across-teams is therefore *rejected*, not stored — though as a
        raw sqlite error rather than a domain-level ValueError. Pins the real
        behavior; ``record_match`` does not pre-validate for the duplicate.
        """
        import sqlite3

        shared = 45000
        radiant = [shared, 45001, 45002, 45003, 45004]
        dire = [shared, 45005, 45006, 45007, 45008]

        with pytest.raises(sqlite3.IntegrityError, match="UNIQUE|PRIMARY"):
            repo.record_match(
                team1_ids=radiant,
                team2_ids=dire,
                winning_team=1,
                guild_id=TEST_GUILD_ID,
            )
