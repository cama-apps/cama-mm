"""Tests for the cama pets schema migrations."""

import sqlite3

import pytest

from infrastructure.schema_manager import SchemaManager


def _connect(db_path: str) -> sqlite3.Connection:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    return conn


def _insert_pet(conn: sqlite3.Connection, discord_id: int, guild_id: int, **overrides):
    values = {
        "discord_id": discord_id,
        "guild_id": guild_id,
        "name": "Testcama",
        "species": "common_cama",
        "adopted_at": 1000,
        "hatched_at": 1000 + 86400,
        "adopt_fee": 20,
        "last_fed_at": 1000 + 86400,
        "hunger_at_last_fed": 100,
    }
    values.update(overrides)
    columns = ", ".join(values)
    placeholders = ", ".join("?" for _ in values)
    conn.execute(
        f"INSERT INTO pets ({columns}) VALUES ({placeholders})",
        tuple(values.values()),
    )


class TestPetMigrations:
    def test_tables_and_indexes_exist(self, repo_db_path):
        with _connect(repo_db_path) as conn:
            tables = {
                row["name"]
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                )
            }
            assert {"pets", "pet_supplies", "pet_refund_windows"} <= tables
            indexes = {
                row["name"]
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'index'"
                )
            }
            assert "idx_pets_one_alive" in indexes
            assert "idx_pets_unannounced_deaths" in indexes

    def test_migration_is_idempotent(self, repo_db_path):
        manager = SchemaManager(repo_db_path)
        with _connect(repo_db_path) as conn:
            cursor = conn.cursor()
            manager._migration_create_pet_tables(cursor)
            manager._migration_add_pet_enabled_to_reminder_preferences(cursor)
            conn.commit()

    def test_one_living_pet_per_player_per_guild(self, repo_db_path):
        with _connect(repo_db_path) as conn:
            _insert_pet(conn, 1, 100)
            with pytest.raises(sqlite3.IntegrityError):
                _insert_pet(conn, 1, 100)

    def test_dead_pet_does_not_block_re_adoption(self, repo_db_path):
        with _connect(repo_db_path) as conn:
            _insert_pet(conn, 1, 100, died_at=90000, death_cause="starvation")
            _insert_pet(conn, 1, 100)  # new living pet is fine
            # And the same player in another guild is independent.
            _insert_pet(conn, 1, 200)

    def test_check_constraints(self, repo_db_path):
        with _connect(repo_db_path) as conn:
            with pytest.raises(sqlite3.IntegrityError):
                _insert_pet(conn, 2, 100, name="")
            with pytest.raises(sqlite3.IntegrityError):
                _insert_pet(conn, 2, 100, hunger_at_last_fed=101)
            with pytest.raises(sqlite3.IntegrityError):
                _insert_pet(conn, 2, 100, hatched_at=999)  # before adopted_at
            with pytest.raises(sqlite3.IntegrityError):
                # Announced death without a death.
                _insert_pet(conn, 2, 100, death_announced_at=5000)
            with pytest.raises(sqlite3.IntegrityError):
                conn.execute(
                    "INSERT INTO pet_supplies "
                    "(discord_id, guild_id, item_id, qty, updated_at) "
                    "VALUES (1, 100, 'tango', -1, 0)"
                )

    def test_refund_window_claim_is_unique(self, repo_db_path):
        with _connect(repo_db_path) as conn:
            conn.execute(
                "INSERT INTO pet_refund_windows "
                "(guild_id, week_key, paid_at, total_paid) VALUES (100, '2026-W30', 1, 50)"
            )
            with pytest.raises(sqlite3.IntegrityError):
                conn.execute(
                    "INSERT INTO pet_refund_windows "
                    "(guild_id, week_key, paid_at, total_paid) "
                    "VALUES (100, '2026-W30', 2, 60)"
                )

    def test_refund_windows_track_pending_announcements(self, repo_db_path):
        with _connect(repo_db_path) as conn:
            columns = {
                row["name"] for row in conn.execute("PRAGMA table_info(pet_refund_windows)")
            }
            assert {"announcement_payload", "announced_at"} <= columns

    def test_reminder_preferences_pet_column(self, repo_db_path):
        with _connect(repo_db_path) as conn:
            columns = {
                row["name"]
                for row in conn.execute("PRAGMA table_info(reminder_preferences)")
            }
            assert "pet_enabled" in columns
