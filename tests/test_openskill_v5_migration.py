"""OpenSkill v5 migration replays persisted positive-gain multipliers."""

import json
import sqlite3

import pytest

from infrastructure.schema_manager import SchemaManager
from openskill_rating_system import CamaOpenSkillSystem
from openskill_replay import OPENSKILL_ALGORITHM_VERSION
from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID

MIGRATION_NAME = "openskill_v5_gain_multiplier_replay"


def test_registration_after_v5_upgrade_stamps_current_algorithm_identity(repo_db_path):
    legacy_player_id = 99498
    new_player_id = 99499

    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            """
            INSERT INTO players (
                discord_id, guild_id, discord_username, initial_mmr,
                os_mu, os_sigma, os_rating_version
            )
            VALUES (?, ?, 'LegacyV4Player', 3000, 25, 8.333, 4)
            """,
            (legacy_player_id, TEST_GUILD_ID),
        )
        conn.execute(
            "ALTER TABLE players RENAME COLUMN os_rating_version TO replaced_rating_version"
        )
        conn.execute(
            "ALTER TABLE players ADD COLUMN os_rating_version INTEGER NOT NULL DEFAULT 3"
        )
        conn.execute("ALTER TABLE players DROP COLUMN replaced_rating_version")
        conn.execute(
            """
            UPDATE players
            SET os_rating_version = 4,
                os_algorithm_fingerprint = 'legacy-v4-fingerprint'
            WHERE discord_id = ? AND guild_id = ?
            """,
            (legacy_player_id, TEST_GUILD_ID),
        )
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            (MIGRATION_NAME,),
        )

    SchemaManager(repo_db_path).initialize()
    PlayerRepository(repo_db_path).add(
        discord_id=new_player_id,
        discord_username="RegisteredAfterV5",
        guild_id=TEST_GUILD_ID,
    )

    with sqlite3.connect(repo_db_path) as conn:
        identity = conn.execute(
            """
            SELECT os_rating_version, os_algorithm_fingerprint
            FROM players
            WHERE discord_id = ? AND guild_id = ?
            """,
            (new_player_id, TEST_GUILD_ID),
        ).fetchone()

    assert identity == (
        OPENSKILL_ALGORITHM_VERSION,
        CamaOpenSkillSystem.algorithm_fingerprint(),
    )


def test_v5_migration_replays_recorded_gain_multipliers(repo_db_path):
    player_ids = list(range(99500, 99510))
    match_id = 99600

    with sqlite3.connect(repo_db_path) as conn:
        conn.executemany(
            """
            INSERT INTO players (
                discord_id, guild_id, discord_username, initial_mmr,
                os_mu, os_sigma, os_rating_version
            )
            VALUES (?, ?, ?, 3000, 999, 1, 4)
            """,
            [
                (player_id, TEST_GUILD_ID, f"GainReplay{player_id}")
                for player_id in player_ids
            ],
        )
        conn.execute(
            """
            INSERT INTO matches (
                match_id, guild_id, team1_players, team2_players,
                winning_team, match_date
            )
            VALUES (?, ?, ?, ?, 1, '2026-08-10 12:00:00')
            """,
            (
                match_id,
                TEST_GUILD_ID,
                json.dumps(player_ids[:5]),
                json.dumps(player_ids[5:]),
            ),
        )
        for index, player_id in enumerate(player_ids):
            team_number = 1 if index < 5 else 2
            multiplier = 1.10 if player_id == player_ids[0] else 1.0
            conn.execute(
                """
                INSERT INTO match_participants (
                    match_id, guild_id, discord_id, team_number, won
                )
                VALUES (?, ?, ?, ?, ?)
                """,
                (
                    match_id,
                    TEST_GUILD_ID,
                    player_id,
                    team_number,
                    int(team_number == 1),
                ),
            )
            conn.execute(
                """
                INSERT INTO rating_history (
                    discord_id, guild_id, match_id, timestamp,
                    team_number, won, os_mu_before, os_mu_after,
                    os_sigma_before, os_sigma_after,
                    os_algorithm_version, low_priority_gain_multiplier
                )
                VALUES (?, ?, ?, '2026-08-10 12:00:00', ?, ?, 999, 999, 1, 1, 4, ?)
                """,
                (
                    player_id,
                    TEST_GUILD_ID,
                    match_id,
                    team_number,
                    int(team_number == 1),
                    multiplier,
                ),
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
        history = conn.execute(
            """
            SELECT discord_id, os_mu_before, os_mu_after,
                   os_algorithm_version, os_algorithm_fingerprint,
                   low_priority_gain_multiplier
            FROM rating_history
            WHERE guild_id = ? AND match_id = ?
            ORDER BY discord_id
            """,
            (TEST_GUILD_ID, match_id),
        ).fetchall()

    assert OPENSKILL_ALGORITHM_VERSION == 5
    assert migration is not None
    assert len(history) == 10
    target, peer = history[:2]
    assert target["low_priority_gain_multiplier"] == pytest.approx(1.10)
    assert peer["low_priority_gain_multiplier"] == pytest.approx(1.0)
    assert target["os_mu_after"] - target["os_mu_before"] == pytest.approx(
        (peer["os_mu_after"] - peer["os_mu_before"]) * 1.10
    )
    assert all(
        row["os_algorithm_version"] == OPENSKILL_ALGORITHM_VERSION
        and row["os_algorithm_fingerprint"]
        for row in history
    )
