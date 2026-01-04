"""
End-to-end tests for guild-specific match state tracking.
"""

import os
import tempfile
import time

import pytest

from database import Database
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService


@pytest.fixture
def match_test_db():
    """Create a temporary Database for match service tests."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    db = Database(db_path)
    yield db
    try:
        import sqlite3

        sqlite3.connect(db_path).close()
    except Exception:
        pass
    try:
        os.unlink(db_path)
    except PermissionError:
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except Exception:
            pass


def create_test_players(db, start_id=60000, count=10):
    """Helper function to create test players."""
    player_ids = list(range(start_id, start_id + count))
    for idx, pid in enumerate(player_ids):
        db.add_player(
            discord_id=pid,
            discord_username=f"TestGuildPlayer{pid}",
            initial_mmr=1600 + idx * 10,
            glicko_rating=1600.0 + idx * 2,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
    return player_ids


class TestGuildIdMatchState:
    """Ensure shuffle state is tracked per guild."""

    def test_shuffle_and_record_with_guild_id(self, match_test_db):
        player_repo = PlayerRepository(match_test_db.db_path)
        match_repo = MatchRepository(match_test_db.db_path)
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo)
        player_ids = create_test_players(match_test_db, start_id=50000)
        guild_id = 42

        match_service.shuffle_players(player_ids, guild_id=guild_id)
        assert match_service.get_last_shuffle(guild_id) is not None

        result = match_service.record_match("radiant", guild_id=guild_id)
        assert result["winning_team"] == "radiant"
        assert match_service.get_last_shuffle(guild_id) is None

    def test_shuffle_and_record_without_guild_id(self, match_test_db):
        player_repo = PlayerRepository(match_test_db.db_path)
        match_repo = MatchRepository(match_test_db.db_path)
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo)
        player_ids = create_test_players(match_test_db, start_id=60000)

        match_service.shuffle_players(player_ids)
        assert match_service.get_last_shuffle(None) is not None

        result = match_service.record_match("dire")
        assert result["winning_team"] == "dire"
        assert match_service.get_last_shuffle(None) is None

    def test_shuffle_and_record_different_guilds(self, match_test_db):
        player_repo = PlayerRepository(match_test_db.db_path)
        match_repo = MatchRepository(match_test_db.db_path)
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo)
        player_ids = create_test_players(match_test_db, start_id=70000)

        match_service.shuffle_players(player_ids, guild_id=1)
        with pytest.raises(ValueError, match="No recent shuffle found."):
            match_service.record_match("radiant", guild_id=2)

        assert match_service.get_last_shuffle(1) is not None


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
