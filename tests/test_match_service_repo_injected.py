import os
import tempfile

import pytest

from domain.models.team import Team
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService


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


def test_goodness_score_respects_role_matchup_weight(monkeypatch):
    """Ensure goodness_score uses the weighted role delta (0.2 default)."""
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

        # Seed deterministic players with fixed roles (no off-role penalties).
        team1_defs = [
            (8001, "RadiantCarry", 2000, ["1"]),
            (8002, "RadiantMid", 1500, ["2"]),
            (8003, "RadiantOfflane", 1000, ["3"]),
            (8004, "RadiantSoft", 1000, ["4"]),
            (8005, "RadiantHard", 1000, ["5"]),
        ]
        team2_defs = [
            (8006, "DireCarry", 1400, ["1"]),
            (8007, "DireMid", 1500, ["2"]),
            (8008, "DireOfflane", 1900, ["3"]),
            (8009, "DireSoft", 1000, ["4"]),
            (8010, "DireHard", 1000, ["5"]),
        ]
        all_defs = team1_defs + team2_defs
        for pid, name, mmr, roles in all_defs:
            player_repo.add(
                discord_id=pid,
                discord_username=name,
                preferred_roles=roles,
                initial_mmr=mmr,
                glicko_rating=None,
                glicko_rd=None,
                glicko_volatility=None,
            )

        player_ids = [pid for pid, _, _, _ in all_defs]

        # Build deterministic teams using the exact player objects provided to shuffle_players
        # (so player_id_map resolves correctly by object identity).
        def fake_shuffle(_players):
            team1_players = _players[:5]
            team2_players = _players[5:]
            return (
                Team(team1_players, role_assignments=["1", "2", "3", "4", "5"]),
                Team(team2_players, role_assignments=["1", "2", "3", "4", "5"]),
            )

        monkeypatch.setattr(service.shuffler, "shuffle", fake_shuffle)

        result = service.shuffle_players(player_ids, guild_id=1)

        # value diff = |6500 - 6800| = 300
        # role delta = 400; weighted by 0.3 -> 120
        # off-role penalty and exclusion penalty = 0
        assert result["goodness_score"] == pytest.approx(380)
    finally:
        try:
            os.unlink(db_path)
        except OSError:
            pass
