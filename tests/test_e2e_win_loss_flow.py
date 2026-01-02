"""
End-to-end tests for win/loss recording, stats, and leaderboard reporting.
"""

import pytest

# Ensure e2e fixtures (e2e_test_db) are registered
pytest_plugins = ["tests.conftest_e2e"]

from database import Database
from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService


def _create_players(db: Database, start_id: int = 91001, count: int = 10):
    ids = list(range(start_id, start_id + count))
    for idx, pid in enumerate(ids):
        db.add_player(
            discord_id=pid,
            discord_username=f"E2EPlayer{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0 + idx,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return ids


def _sort_leaderboard_like_command(repo: PlayerRepository):
    rating_system = repo  # placeholder to mirror access; actual conversion done inline below
    with repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "SELECT discord_id, discord_username, wins, losses, glicko_rating, COALESCE(jopacoin_balance, 0) as jopacoin_balance FROM players"
        )
        rows = cursor.fetchall()

    players = []
    for row in rows:
        jopacoin = row["jopacoin_balance"] or 0
        wins = row["wins"] or 0
        rating_value = row["glicko_rating"]
        # The command sorts by jopacoin, then wins, then rating
        players.append((row["discord_id"], jopacoin, wins, rating_value or 0))

    players.sort(key=lambda x: (x[1], x[2], x[3]), reverse=True)
    return players


def test_record_to_stats_and_leaderboard_flow(e2e_test_db):
    player_ids = _create_players(e2e_test_db)
    radiant = player_ids[:5]
    dire = player_ids[5:]

    # Record a Radiant win
    e2e_test_db.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team="radiant",
    )

    repo = PlayerRepository(e2e_test_db.db_path)
    service = PlayerService(repo)

    winner_stats = service.get_stats(radiant[0])
    loser_stats = service.get_stats(dire[0])

    assert winner_stats["player"].wins == 1
    assert winner_stats["player"].losses == 0
    assert winner_stats["win_rate"] == pytest.approx(100.0)

    assert loser_stats["player"].wins == 0
    assert loser_stats["player"].losses == 1
    assert loser_stats["win_rate"] == pytest.approx(0.0)

    # Leaderboard should place winners above losers when jopacoin is equal
    leaderboard = _sort_leaderboard_like_command(repo)
    top_wins = [entry[2] for entry in leaderboard[:5]]
    bottom_wins = [entry[2] for entry in leaderboard[5:]]

    assert all(w == 1 for w in top_wins)
    assert all(w == 0 for w in bottom_wins)


def test_multi_match_accumulation_and_nonparticipant(e2e_test_db):
    player_ids = _create_players(e2e_test_db, start_id=92001)
    radiant = player_ids[:5]
    dire = player_ids[5:]
    bench = 93001
    e2e_test_db.add_player(
        discord_id=bench,
        discord_username="BenchPlayer",
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )

    # Two matches, alternating winners
    e2e_test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant")
    e2e_test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire")

    repo = PlayerRepository(e2e_test_db.db_path)
    service = PlayerService(repo)

    for pid in radiant + dire:
        stats = service.get_stats(pid)
        assert stats["player"].wins == 1
        assert stats["player"].losses == 1
        assert stats["win_rate"] == pytest.approx(50.0)

    bench_stats = service.get_stats(bench)
    assert bench_stats["player"].wins == 0
    assert bench_stats["player"].losses == 0
    assert bench_stats["win_rate"] is None

