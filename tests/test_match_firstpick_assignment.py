"""
Tests for firstpick team assignment in match shuffling.
"""

from types import SimpleNamespace

import pytest

from database import Database
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService

TEST_GUILD_ID = 123


class TestFirstpickAssignment:
    """Test that firstpick team is randomly assigned between Radiant and Dire."""

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
        player_ids = [7001, 7002, 7003, 7004, 7005, 7006, 7007, 7008, 7009, 7010]
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

    @pytest.fixture
    def match_service(self, test_db, player_repo):
        """Create a MatchService instance."""
        match_repo = MatchRepository(test_db.db_path)
        return MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    @pytest.mark.parametrize(
        ("choice_index", "expected"),
        [(0, "Radiant"), (1, "Dire")],
    )
    def test_firstpick_choice_is_returned_and_persisted(
        self, match_service, test_db, test_players, monkeypatch, choice_index, expected
    ):
        """Both random choices are returned and persisted in match state."""
        captured = {}

        def choose(options):
            captured["options"] = list(options)
            return options[choice_index]

        monkeypatch.setattr(
            "services.match.shuffle_pending_mixin.random",
            SimpleNamespace(random=lambda: 0.25, choice=choose),
        )

        result = match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)
        state = match_service.get_last_shuffle(TEST_GUILD_ID)

        assert captured["options"] == ["Radiant", "Dire"]
        assert result["first_pick_team"] == expected
        assert state is not None
        assert state.first_pick_team == expected

    def test_firstpick_assignment_multiple_guilds(self, match_service, test_db, player_repo, test_players):
        """Test that firstpick assignment works correctly for different guilds."""
        # Add players to guild 100 and 200
        for pid in test_players:
            player_repo.add(
                discord_id=pid + 1000,  # Offset to avoid collision with existing test_players
                discord_username=f"Player{pid + 1000}",
                guild_id=100,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_repo.add(
                discord_id=pid + 2000,
                discord_username=f"Player{pid + 2000}",
                guild_id=200,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        player_ids_100 = [pid + 1000 for pid in test_players]
        player_ids_200 = [pid + 2000 for pid in test_players]

        result1 = match_service.shuffle_players(player_ids_100, guild_id=100)
        result2 = match_service.shuffle_players(player_ids_200, guild_id=200)

        assert "first_pick_team" in result1
        assert "first_pick_team" in result2
        assert result1["first_pick_team"] in ("Radiant", "Dire")
        assert result2["first_pick_team"] in ("Radiant", "Dire")

        # Verify each guild has its own state with firstpick
        state1 = match_service.get_last_shuffle(100)
        state2 = match_service.get_last_shuffle(200)

        assert state1 is not None
        assert state2 is not None
        assert state1.first_pick_team == result1["first_pick_team"]
        assert state2.first_pick_team == result2["first_pick_team"]


class TestFirstpickEndToEnd:
    """End-to-end tests for firstpick assignment through the full workflow."""

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
        player_ids = [8001, 8002, 8003, 8004, 8005, 8006, 8007, 8008, 8009, 8010]
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

    @pytest.fixture
    def match_service(self, test_db, player_repo):
        """Create a MatchService instance."""
        match_repo = MatchRepository(test_db.db_path)
        return MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    def test_firstpick_persists_through_record_workflow(self, match_service, test_db, test_players):
        """
        Test that firstpick assignment persists through the shuffle and record workflow.

        This is an end-to-end test that verifies firstpick is assigned during shuffle,
        stored in state, and available until the match is recorded.
        """
        # Shuffle players
        result = match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)

        # Verify firstpick is in the result
        assert "first_pick_team" in result
        first_pick = result["first_pick_team"]
        assert first_pick in ("Radiant", "Dire")

        # Verify firstpick is in the stored state
        state = match_service.get_last_shuffle(TEST_GUILD_ID)
        assert state is not None
        assert state.first_pick_team == first_pick

        # Record the match (state should be cleared after)
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # Verify state is cleared
        assert match_service.get_last_shuffle(TEST_GUILD_ID) is None

