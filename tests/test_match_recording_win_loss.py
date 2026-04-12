"""
Additional unit tests for win/loss recording edge cases and integrity.
"""

import pytest


@pytest.fixture
def player_ids(test_db_with_schema):
    """Create 10 players in the database."""
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


@pytest.mark.parametrize(
    ("winning_team", "winning_slice", "losing_slice"),
    [
        ("radiant", slice(0, 5), slice(5, 10)),
        ("dire", slice(5, 10), slice(0, 5)),
    ],
    ids=["radiant_wins", "dire_wins"],
)
def test_win_updates_wins_and_losses(
    test_db_with_schema, player_ids, winning_team, winning_slice, losing_slice
):
    radiant = player_ids[:5]
    dire = player_ids[5:]

    test_db_with_schema.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team=winning_team,
    )

    for pid in player_ids[winning_slice]:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 1
        assert losses == 0

    for pid in player_ids[losing_slice]:
        wins, losses = _fetch_wins_losses(test_db_with_schema, pid)
        assert wins == 0
        assert losses == 1


def test_multiple_matches_accumulate_correctly(test_db_with_schema, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

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


def test_existing_wins_losses_are_incremented(test_db_with_schema, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

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


def test_participants_table_has_correct_side_and_won_flags(test_db_with_schema, player_ids):
    radiant = player_ids[:5]
    dire = player_ids[5:]

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
