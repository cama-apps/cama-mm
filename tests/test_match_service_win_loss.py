"""
Integration tests for MatchService win/loss recording.

NOTE: This file is superseded by test_match_e2e.py which consolidates
match recording tests. This file is kept for now for compatibility.
"""

import pytest

from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


def _add_players(player_repo: PlayerRepository, start_id: int = 94001, guild_id: int = TEST_GUILD_ID):
    ids = list(range(start_id, start_id + 10))
    for idx, pid in enumerate(ids):
        player_repo.add(
            discord_id=pid,
            discord_username=f"MSPlayer{pid}",
            guild_id=guild_id,
            initial_mmr=1500,
            glicko_rating=1500.0 + idx,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return ids


def _set_last_shuffle(service: MatchService, radiant_ids, dire_ids, guild_id=TEST_GUILD_ID):
    service.set_last_shuffle(
        guild_id,
        {
            "radiant_team_ids": radiant_ids,
            "dire_team_ids": dire_ids,
            "excluded_player_ids": [],
        },
    )


def test_record_match_updates_wins_and_clears_state(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    player_ids = _add_players(player_repo)
    radiant = player_ids[:5]
    dire = player_ids[5:]

    _set_last_shuffle(match_service, radiant, dire, TEST_GUILD_ID)
    result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    assert result["winning_team"] == "radiant"
    for pid in radiant:
        player = player_repo.get_by_id(pid, TEST_GUILD_ID)
        assert player.wins == 1
        assert player.losses == 0
    for pid in dire:
        player = player_repo.get_by_id(pid, TEST_GUILD_ID)
        assert player.wins == 0
        assert player.losses == 1

    # State should be cleared after successful record
    assert match_service.get_last_shuffle(TEST_GUILD_ID) is None


def test_record_match_without_shuffle_fails(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    player_ids = _add_players(player_repo, start_id=95001)
    radiant = player_ids[:5]
    dire = player_ids[5:]

    # No last shuffle set
    with pytest.raises(ValueError):
        match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    # Ensure no wins/losses were written
    for pid in radiant + dire:
        player = player_repo.get_by_id(pid, TEST_GUILD_ID)
        assert player.wins == 0
        assert player.losses == 0


def test_double_record_prevented(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    match_repo = MatchRepository(repo_db_path)
    match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    player_ids = _add_players(player_repo, start_id=96001)
    radiant = player_ids[:5]
    dire = player_ids[5:]

    _set_last_shuffle(match_service, radiant, dire, TEST_GUILD_ID)
    match_service.record_match("dire", guild_id=TEST_GUILD_ID)

    # Second call without resetting shuffle should fail
    with pytest.raises(ValueError):
        match_service.record_match("dire", guild_id=TEST_GUILD_ID)

    for pid in radiant:
        player = player_repo.get_by_id(pid, TEST_GUILD_ID)
        assert player.wins == 0
        assert player.losses == 1
    for pid in dire:
        player = player_repo.get_by_id(pid, TEST_GUILD_ID)
        assert player.wins == 1
        assert player.losses == 0
