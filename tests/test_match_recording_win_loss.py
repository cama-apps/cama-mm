"""
Additional unit tests for win/loss recording edge cases and integrity.
"""

import os
import tempfile
import time

import pytest

from database import Database


@pytest.fixture
def test_db():
    """Temporary database for win/loss recording tests."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    db = Database(db_path)
    yield db
    try:
        import sqlite3

        sqlite3.connect(db_path).close()
    except Exception:
        pass
    time.sleep(0.05)
    try:
        os.unlink(db_path)
    except Exception:
        pass


@pytest.fixture
def player_ids(test_db):
    """Create 10 players in the database."""
    ids = list(range(11001, 11011))
    for pid in ids:
        test_db.add_player(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return ids


def _fetch_wins_losses(test_db, discord_id):
    player = test_db.get_player(discord_id)
    return player.wins, player.losses


def test_radiant_win_updates_wins_and_losses(test_db, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

    test_db.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team="radiant",
    )

    for pid in radiant:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 1
        assert losses == 0

    for pid in dire:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 0
        assert losses == 1


def test_dire_win_updates_wins_and_losses(test_db, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

    test_db.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team="dire",
    )

    for pid in dire:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 1
        assert losses == 0

    for pid in radiant:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 0
        assert losses == 1


def test_multiple_matches_accumulate_correctly(test_db, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

    # Radiant wins twice, Dire wins once
    test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant")
    test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant")
    test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire")

    for pid in radiant:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 2
        assert losses == 1

    for pid in dire:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 1
        assert losses == 2


def test_existing_wins_losses_are_incremented(test_db, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

    # Seed some prior stats
    with test_db.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE players SET wins = 2, losses = 3 WHERE discord_id IN ({})".format(",".join("?" * len(radiant))),
            radiant,
        )
        cursor.execute(
            "UPDATE players SET wins = 1, losses = 4 WHERE discord_id IN ({})".format(",".join("?" * len(dire))),
            dire,
        )

    test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire")

    for pid in radiant:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 2  # unchanged wins
        assert losses == 4  # incremented loss

    for pid in dire:
        wins, losses = _fetch_wins_losses(test_db, pid)
        assert wins == 2  # incremented win
        assert losses == 4  # unchanged losses


def test_participants_table_has_correct_side_and_won_flags(test_db, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

    match_id = test_db.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team="radiant",
    )

    conn = test_db.get_connection()
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
