"""Tests for dig-system schema migrations."""

import json
import sqlite3

from infrastructure.schema_manager import SchemaManager


def _seed_tunnel(db_path: str, discord_id: int, guild_id: int, boss_progress: str) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "INSERT INTO tunnels (discord_id, guild_id, boss_progress) "
            "VALUES (?, ?, ?)",
            (discord_id, guild_id, boss_progress),
        )
        conn.commit()
    finally:
        conn.close()


def _read_boss_progress(db_path: str, discord_id: int, guild_id: int) -> str | None:
    conn = sqlite3.connect(db_path)
    try:
        row = conn.execute(
            "SELECT boss_progress FROM tunnels "
            "WHERE discord_id = ? AND guild_id = ?",
            (discord_id, guild_id),
        ).fetchone()
        return row[0] if row else None
    finally:
        conn.close()


def _run_clear_active_migration(db_path: str) -> None:
    """Invoke the corrective migration directly on an already-initialized DB."""
    manager = SchemaManager(db_path)
    conn = sqlite3.connect(db_path)
    try:
        cursor = conn.cursor()
        manager._migration_clear_active_boss_ids_for_pool_reroll(cursor)
        conn.commit()
    finally:
        conn.close()


class TestClearActiveBossIdsMigration:
    """Covers _migration_clear_active_boss_ids_for_pool_reroll."""

    def test_clears_active_boss_ids_only(self, repo_db_path):
        seeded = json.dumps({
            "25":  {"boss_id": "grothak",             "status": "active"},
            "50":  {"boss_id": "crystalia",           "status": "defeated"},
            "75":  {"boss_id": "magmus_rex",          "status": "phase1_defeated"},
            "100": {"boss_id": "void_warden",         "status": "active"},
        })
        _seed_tunnel(repo_db_path, discord_id=111, guild_id=222, boss_progress=seeded)

        _run_clear_active_migration(repo_db_path)

        result = json.loads(_read_boss_progress(repo_db_path, 111, 222))
        assert result["25"] == {"boss_id": "", "status": "active"}
        assert result["50"] == {"boss_id": "crystalia", "status": "defeated"}
        assert result["75"] == {"boss_id": "magmus_rex", "status": "phase1_defeated"}
        assert result["100"] == {"boss_id": "", "status": "active"}

    def test_idempotent_when_boss_id_already_empty(self, repo_db_path):
        seeded = json.dumps({
            "25": {"boss_id": "", "status": "active"},
            "50": {"boss_id": "crystalia", "status": "defeated"},
        })
        _seed_tunnel(repo_db_path, discord_id=333, guild_id=444, boss_progress=seeded)

        _run_clear_active_migration(repo_db_path)
        _run_clear_active_migration(repo_db_path)

        result = json.loads(_read_boss_progress(repo_db_path, 333, 444))
        assert result == {
            "25": {"boss_id": "", "status": "active"},
            "50": {"boss_id": "crystalia", "status": "defeated"},
        }

    def test_handles_legacy_string_entries(self, repo_db_path):
        seeded = json.dumps({"25": "active", "50": "defeated"})
        _seed_tunnel(repo_db_path, discord_id=555, guild_id=666, boss_progress=seeded)

        _run_clear_active_migration(repo_db_path)

        result = json.loads(_read_boss_progress(repo_db_path, 555, 666))
        assert result == {"25": "active", "50": "defeated"}

    def test_skips_tunnels_with_invalid_json(self, repo_db_path):
        _seed_tunnel(repo_db_path, discord_id=777, guild_id=888, boss_progress="{not json")

        _run_clear_active_migration(repo_db_path)

        assert _read_boss_progress(repo_db_path, 777, 888) == "{not json"

    def test_skips_tunnels_with_null_boss_progress(self, repo_db_path):
        conn = sqlite3.connect(repo_db_path)
        try:
            conn.execute(
                "INSERT INTO tunnels (discord_id, guild_id, boss_progress) VALUES (?, ?, NULL)",
                (999, 1000),
            )
            conn.commit()
        finally:
            conn.close()

        _run_clear_active_migration(repo_db_path)

        assert _read_boss_progress(repo_db_path, 999, 1000) is None

    def test_runs_during_normal_initialization(self, tmp_path):
        """Fresh init must register the migration and complete cleanly."""
        db_path = str(tmp_path / "fresh.db")
        SchemaManager(db_path).initialize()

        conn = sqlite3.connect(db_path)
        try:
            applied = {
                row[0]
                for row in conn.execute("SELECT name FROM schema_migrations").fetchall()
            }
        finally:
            conn.close()
        assert "clear_active_boss_ids_for_pool_reroll" in applied
