"""
Unit tests for win/loss reporting and stats calculation.
"""

import pytest

from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService

TEST_GUILD_ID = 12345


def _set_wins_losses(repo: PlayerRepository, discord_id: int, wins: int, losses: int, guild_id: int = TEST_GUILD_ID) -> None:
    with repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE players SET wins = ?, losses = ? WHERE discord_id = ? AND guild_id = ?",
            (wins, losses, discord_id, guild_id),
        )


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


def _add_player(repo: PlayerRepository, discord_id: int, username: str = None, guild_id: int = TEST_GUILD_ID) -> None:
    repo.add(
        discord_id=discord_id,
        discord_username=username or f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=2000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )


def test_get_stats_win_rate_variations(player_repo):
    player_id = 71001
    _add_player(player_repo, player_id)
    service = PlayerService(player_repo)

    # No games -> win_rate None
    stats = service.get_stats(player_id, TEST_GUILD_ID)
    assert stats["win_rate"] is None

    # 100% win rate
    _set_wins_losses(player_repo, player_id, wins=5, losses=0)
    stats = service.get_stats(player_id, TEST_GUILD_ID)
    assert stats["win_rate"] == pytest.approx(100.0)

    # 0% win rate
    _set_wins_losses(player_repo, player_id, wins=0, losses=4)
    stats = service.get_stats(player_id, TEST_GUILD_ID)
    assert stats["win_rate"] == pytest.approx(0.0)

    # 50% win rate
    _set_wins_losses(player_repo, player_id, wins=3, losses=3)
    stats = service.get_stats(player_id, TEST_GUILD_ID)
    assert stats["win_rate"] == pytest.approx(50.0)
