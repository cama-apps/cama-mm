"""OpenSkill v3's durable migration-time chronological replay."""

from __future__ import annotations

import json
import sqlite3

import pytest

from infrastructure.schema_manager import SchemaManager
from tests.conftest import TEST_GUILD_ID

MIGRATION_NAME = "openskill_v3_durable_native_replay"


def test_pending_migration_backfills_history_players_and_predictions(
    repo_db_path,
):
    player_ids = list(range(81000, 81010))
    match_id = 91000
    fantasy = [5.0, 10.0, 15.0, 20.0, 30.0] * 2

    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        conn.executemany(
            """
            INSERT INTO players (
                discord_id, guild_id, discord_username, initial_mmr,
                os_mu, os_sigma
            )
            VALUES (?, ?, ?, 3000, 999, 1)
            """,
            [
                (player_id, TEST_GUILD_ID, f"Replay{player_id}")
                for player_id in player_ids
            ],
        )
        conn.execute(
            """
            INSERT INTO matches (
                match_id, guild_id, team1_players, team2_players,
                winning_team, match_date
            )
            VALUES (?, ?, ?, ?, 1, '2026-01-01 12:00:00')
            """,
            (
                match_id,
                TEST_GUILD_ID,
                json.dumps(player_ids[:5]),
                json.dumps(player_ids[5:]),
            ),
        )
        conn.executemany(
            """
            INSERT INTO match_participants (
                match_id, guild_id, discord_id, team_number, side,
                won, fantasy_points
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            [
                (
                    match_id,
                    TEST_GUILD_ID,
                    player_id,
                    1 if index < 5 else 2,
                    "radiant" if index < 5 else "dire",
                    1 if index < 5 else 0,
                    fantasy[index],
                )
                for index, player_id in enumerate(player_ids)
            ],
        )
        # Deliberately leave one history row missing and corrupt the rest.
        conn.executemany(
            """
            INSERT INTO rating_history (
                discord_id, guild_id, match_id, team_number, won,
                os_mu_before, os_mu_after, os_sigma_before, os_sigma_after
            )
            VALUES (?, ?, ?, ?, ?, 999, 999, 1, 1)
            """,
            [
                (
                    player_id,
                    TEST_GUILD_ID,
                    match_id,
                    1 if index < 5 else 2,
                    1 if index < 5 else 0,
                )
                for index, player_id in enumerate(player_ids[:-1])
            ],
        )
        conn.execute(
            """
            INSERT INTO match_predictions (
                match_id, expected_radiant_win_prob
            )
            VALUES (?, 0.5)
            """,
            (match_id,),
        )
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            (MIGRATION_NAME,),
        )

    SchemaManager(repo_db_path).initialize()

    with sqlite3.connect(repo_db_path) as conn:
        conn.row_factory = sqlite3.Row
        migration = conn.execute(
            "SELECT 1 FROM schema_migrations WHERE name = ?",
            (MIGRATION_NAME,),
        ).fetchone()
        players = conn.execute(
            """
            SELECT os_mu, os_sigma, os_rating_version,
                   os_algorithm_fingerprint
            FROM players
            WHERE guild_id = ? AND discord_id BETWEEN 81000 AND 81009
            ORDER BY discord_id
            """,
            (TEST_GUILD_ID,),
        ).fetchall()
        history = conn.execute(
            """
            SELECT *
            FROM rating_history
            WHERE guild_id = ? AND match_id = ?
            ORDER BY discord_id
            """,
            (TEST_GUILD_ID, match_id),
        ).fetchall()
        prediction = conn.execute(
            """
            SELECT openskill_radiant_win_prob,
                   openskill_raw_radiant_win_prob,
                   openskill_algorithm_version,
                   openskill_algorithm_fingerprint
            FROM match_predictions
            WHERE match_id = ?
            """,
            (match_id,),
        ).fetchone()
        revision = conn.execute(
            """
            SELECT revision
            FROM openskill_rating_revisions
            WHERE guild_id = ?
            """,
            (TEST_GUILD_ID,),
        ).fetchone()

    assert migration is not None
    assert len(players) == 10
    assert all(row["os_mu"] != 999 for row in players)
    assert all(row["os_rating_version"] == 3 for row in players)
    assert all(row["os_algorithm_fingerprint"] for row in players)
    assert len(history) == 10
    assert all(row["os_algorithm_version"] == 3 for row in history)
    assert all(row["os_algorithm_fingerprint"] for row in history)
    assert all(row["fantasy_weight"] is not None for row in history)
    assert prediction["openskill_algorithm_version"] == 3
    assert prediction["openskill_algorithm_fingerprint"]
    assert prediction["openskill_radiant_win_prob"] == pytest.approx(0.5)
    assert prediction["openskill_raw_radiant_win_prob"] == pytest.approx(0.5)
    assert revision["revision"] >= 1
