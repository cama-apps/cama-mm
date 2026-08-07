"""OpenSkill v4 migration replays the persisted historical streak curve."""

from __future__ import annotations

import json
import sqlite3
from datetime import datetime, timedelta

import pytest

from infrastructure.schema_manager import SchemaManager
from openskill_replay import OPENSKILL_ALGORITHM_VERSION, replay_openskill
from tests.conftest import TEST_GUILD_ID

MIGRATION_NAME = "openskill_v4_streak_replay"


def test_streak_threshold_column_defaults_to_legacy_3(repo_db_path):
    """rating_history.streak_threshold exists and backfills legacy rows to 3."""
    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        columns = {
            row["name"] for row in conn.execute("PRAGMA table_info(rating_history)")
        }
        assert "streak_threshold" in columns

        conn.execute(
            """
            INSERT INTO players (discord_id, guild_id, discord_username)
            VALUES (98999, ?, 'ThresholdDefault')
            """,
            (TEST_GUILD_ID,),
        )
        conn.execute(
            """
            INSERT INTO rating_history (discord_id, guild_id, rating, won)
            VALUES (98999, ?, 1500.0, 1)
            """,
            (TEST_GUILD_ID,),
        )
        row = conn.execute(
            """
            SELECT streak_threshold FROM rating_history
            WHERE discord_id = 98999 AND guild_id = ?
            """,
            (TEST_GUILD_ID,),
        ).fetchone()
        assert row["streak_threshold"] == 3


def test_v4_migration_replays_recorded_streak_rates_idempotently(repo_db_path):
    player_ids = list(range(99000, 99010))
    players = [
        {
            "discord_id": player_id,
            "guild_id": TEST_GUILD_ID,
            "initial_mmr": 3000,
        }
        for player_id in player_ids
    ]
    matches = []
    participants_by_match = {}

    with sqlite3.connect(repo_db_path) as conn:
        conn.executemany(
            """
            INSERT INTO players (
                discord_id, guild_id, discord_username, initial_mmr,
                os_mu, os_sigma, os_rating_version
            )
            VALUES (?, ?, ?, 3000, 999, 1, 3)
            """,
            [
                (player_id, TEST_GUILD_ID, f"StreakReplay{player_id}")
                for player_id in player_ids
            ],
        )

        for offset, streak_rate in enumerate((0.10, 0.10, 0.10, 0.30)):
            match_id = 99100 + offset
            match_date = (
                datetime(2026, 1, 1) + timedelta(days=offset)
            ).isoformat(sep=" ")
            match = {
                "match_id": match_id,
                "guild_id": TEST_GUILD_ID,
                "winning_team": 1,
                "match_date": match_date,
                "team1_players": player_ids[:5],
                "team2_players": player_ids[5:],
                "streak_multiplier_per_game": streak_rate,
            }
            matches.append(match)
            conn.execute(
                """
                INSERT INTO matches (
                    match_id, guild_id, team1_players, team2_players,
                    winning_team, match_date
                )
                VALUES (?, ?, ?, ?, 1, ?)
                """,
                (
                    match_id,
                    TEST_GUILD_ID,
                    json.dumps(player_ids[:5]),
                    json.dumps(player_ids[5:]),
                    match_date,
                ),
            )
            participants = []
            for index, player_id in enumerate(player_ids):
                team_number = 1 if index < 5 else 2
                won = int(team_number == 1)
                participant = {
                    "discord_id": player_id,
                    "team_number": team_number,
                    "fantasy_points": None,
                }
                participants.append(participant)
                conn.execute(
                    """
                    INSERT INTO match_participants (
                        match_id, guild_id, discord_id, team_number, won
                    )
                    VALUES (?, ?, ?, ?, ?)
                    """,
                    (match_id, TEST_GUILD_ID, player_id, team_number, won),
                )
                conn.execute(
                    """
                    INSERT INTO rating_history (
                        discord_id, guild_id, match_id, timestamp,
                        team_number, won, os_mu_before, os_mu_after,
                        os_sigma_before, os_sigma_after,
                        os_algorithm_version, streak_multiplier_per_game
                    )
                    VALUES (?, ?, ?, ?, ?, ?, 999, 999, 1, 1, 3, ?)
                    """,
                    (
                        player_id,
                        TEST_GUILD_ID,
                        match_id,
                        match_date,
                        team_number,
                        won,
                        streak_rate,
                    ),
                )
            participants_by_match[match_id] = participants

        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            (MIGRATION_NAME,),
        )

    expected = replay_openskill(
        players=players,
        matches=matches,
        participants_by_match=participants_by_match,
    )
    assert expected.errors == []

    SchemaManager(repo_db_path).initialize()

    def snapshot():
        with sqlite3.connect(repo_db_path) as conn:
            conn.row_factory = sqlite3.Row
            migration = conn.execute(
                "SELECT 1 FROM schema_migrations WHERE name = ?",
                (MIGRATION_NAME,),
            ).fetchone()
            player_rows = conn.execute(
                """
                SELECT discord_id, os_mu, os_sigma, os_rating_version,
                       os_algorithm_fingerprint
                FROM players
                WHERE guild_id = ? AND discord_id BETWEEN 99000 AND 99009
                ORDER BY discord_id
                """,
                (TEST_GUILD_ID,),
            ).fetchall()
            history_rows = conn.execute(
                """
                SELECT match_id, discord_id, os_mu_before, os_mu_after,
                       os_sigma_before, os_sigma_after, os_algorithm_version,
                       os_algorithm_fingerprint, streak_multiplier_per_game
                FROM rating_history
                WHERE guild_id = ? AND match_id BETWEEN 99100 AND 99103
                ORDER BY match_id, discord_id
                """,
                (TEST_GUILD_ID,),
            ).fetchall()
            prediction_rows = conn.execute(
                """
                SELECT match_id, openskill_algorithm_version,
                       openskill_algorithm_fingerprint,
                       openskill_radiant_win_prob,
                       openskill_raw_radiant_win_prob
                FROM match_predictions
                WHERE match_id BETWEEN 99100 AND 99103
                ORDER BY match_id
                """
            ).fetchall()
        return migration, player_rows, history_rows, prediction_rows

    migration, player_rows, history_rows, prediction_rows = snapshot()
    assert migration is not None
    assert len(player_rows) == 10
    assert len(history_rows) == 40
    assert len(prediction_rows) == 4
    assert all(
        row["os_rating_version"] == OPENSKILL_ALGORITHM_VERSION for row in player_rows
    )
    assert all(
        row["os_algorithm_version"] == OPENSKILL_ALGORITHM_VERSION
        for row in history_rows
    )
    assert all(
        row["openskill_algorithm_version"] == OPENSKILL_ALGORITHM_VERSION
        for row in prediction_rows
    )

    expected_players = {
        player_id: rating
        for (guild_id, player_id), rating in expected.final_ratings.items()
        if guild_id == TEST_GUILD_ID
    }
    for row in player_rows:
        assert (row["os_mu"], row["os_sigma"]) == pytest.approx(
            expected_players[row["discord_id"]]
        )

    expected_history = {
        (row["match_id"], row["discord_id"]): row for row in expected.history_rows
    }
    for row in history_rows:
        expected_row = expected_history[(row["match_id"], row["discord_id"])]
        assert row["os_mu_before"] == pytest.approx(expected_row["os_mu_before"])
        assert row["os_mu_after"] == pytest.approx(expected_row["os_mu_after"])
        assert row["os_sigma_before"] == pytest.approx(expected_row["os_sigma_before"])
        assert row["os_sigma_after"] == pytest.approx(expected_row["os_sigma_after"])

    before_repeat = (
        [tuple(row) for row in player_rows],
        [tuple(row) for row in history_rows],
        [tuple(row) for row in prediction_rows],
    )
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            (MIGRATION_NAME,),
        )
    SchemaManager(repo_db_path).initialize()
    _migration, players_again, history_again, predictions_again = snapshot()
    after_repeat = (
        [tuple(row) for row in players_again],
        [tuple(row) for row in history_again],
        [tuple(row) for row in predictions_again],
    )
    assert after_repeat == before_repeat
