import os
import tempfile

import pytest

from services.match_service import MatchService
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository


def _seed_players(repo: PlayerRepository, count: int = 10):
    for i in range(count):
        pid = 1000 + i
        repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            preferred_roles=["1", "2", "3", "4", "5"],
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return [1000 + i for i in range(count)]


def test_match_service_repo_injected_shuffle_and_record():
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        player_repo = PlayerRepository(db_path)
        match_repo = MatchRepository(db_path)
        service = MatchService(
            player_repo=player_repo,
            match_repo=match_repo,
            use_glicko=False,
            betting_service=None,
        )

        player_ids = _seed_players(player_repo, 10)

        shuffle_result = service.shuffle_players(player_ids, guild_id=1)
        assert shuffle_result["radiant_team"]
        pending = match_repo.get_pending_match(1)
        assert pending is not None

        result = service.record_match("radiant", guild_id=1)
        assert result["match_id"] > 0
        assert match_repo.get_pending_match(1) is None

        recorded = match_repo.get_match(result["match_id"])
        assert recorded is not None
        assert recorded["winning_team"] in (1, 2)
    finally:
        try:
            os.unlink(db_path)
        except OSError:
            pass

