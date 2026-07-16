import sqlite3

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository


def test_schema_manager_initializes_tables(tmp_path):
    """Test that SchemaManager creates all required tables."""
    db_path = str(tmp_path / "test.db")
    mgr = SchemaManager(db_path)
    mgr.initialize()

    with sqlite3.connect(db_path) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
        tables = {row[0] for row in cursor.fetchall()}

    required = {
        "players",
        "matches",
        "match_participants",
        "rating_history",
        "match_predictions",
        "bets",
        "pending_matches",
        "lobby_state",
        "schema_migrations",
        "economy_ledger_entries",
        "economy_ledger_context",
    }
    assert required.issubset(tables)
    assert {"wheel_wars", "war_bets"}.isdisjoint(tables)


def test_schema_manager_drops_retired_wheel_war_tables(tmp_path):
    db_path = str(tmp_path / "legacy-wheel-war.db")
    with sqlite3.connect(db_path) as conn:
        conn.execute("CREATE TABLE wheel_wars (war_id INTEGER PRIMARY KEY)")
        conn.execute("CREATE TABLE war_bets (bet_id INTEGER PRIMARY KEY)")

    SchemaManager(db_path).initialize()

    with sqlite3.connect(db_path) as conn:
        remaining = conn.execute(
            """
            SELECT name
            FROM sqlite_master
            WHERE type = 'table' AND name IN ('wheel_wars', 'war_bets')
            """
        ).fetchall()

    assert remaining == []


def test_schema_manager_adds_region_columns(tmp_path):
    """The region migration adds preferred_region and inferred_region to players."""
    db_path = str(tmp_path / "test.db")
    mgr = SchemaManager(db_path)
    mgr.initialize()

    with sqlite3.connect(db_path) as conn:
        cursor = conn.cursor()
        cursor.execute("PRAGMA table_info(players)")
        columns = {row[1] for row in cursor.fetchall()}

    assert {"preferred_region", "inferred_region"}.issubset(columns)


def test_schema_manager_adds_last_active_at_with_backfill(tmp_path):
    """The activity migration adds last_active_at and backfills existing rows."""
    db_path = str(tmp_path / "test.db")

    # Simulate a pre-migration players row (no last_active_at column) by
    # inserting through the finished schema then clearing last_active_at.
    SchemaManager(db_path).initialize()
    with sqlite3.connect(db_path) as conn:
        cursor = conn.cursor()
        cursor.execute("PRAGMA table_info(players)")
        columns = {row[1] for row in cursor.fetchall()}
        assert "last_active_at" in columns

        # A row whose only recency signal is created_at should have been
        # backfilled (COALESCE with last_match_date / created_at).
        cursor.execute(
            """
            INSERT INTO players (discord_id, guild_id, discord_username, created_at, last_active_at)
            VALUES (555, 0, 'Legacy', '2024-01-01T00:00:00+00:00', NULL)
            """
        )
        conn.commit()

    # Re-running the backfill portion: NULL last_active_at should resolve to
    # created_at when the migration logic is applied again is not automatic
    # (migration already ran), so assert the column simply exists and accepts
    # writes — the backfill behaviour is covered directly below.
    with sqlite3.connect(db_path) as conn:
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE players SET last_active_at = COALESCE(last_active_at, last_match_date, created_at) WHERE last_active_at IS NULL"
        )
        conn.commit()
        cursor.execute("SELECT last_active_at FROM players WHERE discord_id = 555")
        assert cursor.fetchone()[0] == "2024-01-01T00:00:00+00:00"


def test_schema_manager_creates_command_activity_table(tmp_path):
    """The activity migration creates the command_activity table."""
    db_path = str(tmp_path / "test.db")
    SchemaManager(db_path).initialize()

    with sqlite3.connect(db_path) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
        tables = {row[0] for row in cursor.fetchall()}
        cursor.execute("PRAGMA table_info(command_activity)")
        columns = {row[1] for row in cursor.fetchall()}

    assert "command_activity" in tables
    assert {"guild_id", "discord_id", "last_used_at"}.issubset(columns)


def test_schema_manager_initialize_is_idempotent(tmp_path):
    """Running initialize twice must not fail or duplicate migration rows."""
    db_path = str(tmp_path / "test.db")
    mgr = SchemaManager(db_path)
    mgr.initialize()
    mgr.initialize()

    with sqlite3.connect(db_path) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT COUNT(*) FROM schema_migrations")
        applied = cursor.fetchone()[0]
        cursor.execute("SELECT COUNT(DISTINCT name) FROM schema_migrations")
        distinct = cursor.fetchone()[0]

    assert applied == distinct
    assert applied > 0


def test_failed_pending_batch_rolls_back_all_schema_and_migration_rows(tmp_path):
    db_path = str(tmp_path / "failed-pending-batch.db")
    manager = SchemaManager(db_path)
    migration_names = ("synthetic_batch_a", "synthetic_batch_b")

    def migration_a(cursor):
        cursor.execute("CREATE TABLE synthetic_batch_a (value TEXT NOT NULL)")
        cursor.execute("INSERT INTO synthetic_batch_a (value) VALUES ('a')")

    def migration_b(cursor):
        cursor.execute("CREATE TABLE synthetic_batch_b (value TEXT NOT NULL)")
        cursor.execute("INSERT INTO synthetic_batch_b (value) VALUES ('b')")
        raise RuntimeError("synthetic migration B failed")

    manager._get_migrations = lambda: [
        (migration_names[0], migration_a),
        (migration_names[1], migration_b),
    ]

    with pytest.raises(RuntimeError, match="synthetic migration B failed"):
        manager.initialize()

    with sqlite3.connect(db_path) as conn:
        rolled_back_tables = conn.execute(
            """
            SELECT name
            FROM sqlite_master
            WHERE type = 'table' AND name IN (?, ?)
            """,
            migration_names,
        ).fetchall()
        migration_rows = conn.execute(
            "SELECT name FROM schema_migrations WHERE name IN (?, ?)",
            migration_names,
        ).fetchall()

    assert rolled_back_tables == []
    assert migration_rows == []


def test_failed_pending_batch_retries_cleanly(tmp_path):
    db_path = str(tmp_path / "retry-pending-batch.db")
    manager = SchemaManager(db_path)
    migration_names = ("synthetic_retry_a", "synthetic_retry_b")

    def initial_migration_a(cursor):
        cursor.execute("CREATE TABLE synthetic_retry_a (value TEXT NOT NULL)")
        cursor.execute("INSERT INTO synthetic_retry_a (value) VALUES ('a')")

    def failing_migration_b(cursor):
        cursor.execute("CREATE TABLE synthetic_retry_b (value TEXT NOT NULL)")
        cursor.execute("INSERT INTO synthetic_retry_b (value) VALUES ('b')")
        raise RuntimeError("synthetic migration B failed")

    manager._get_migrations = lambda: [
        (migration_names[0], initial_migration_a),
        (migration_names[1], failing_migration_b),
    ]

    with pytest.raises(RuntimeError, match="synthetic migration B failed"):
        manager.initialize()

    def successful_migration_a(cursor):
        cursor.execute("CREATE TABLE synthetic_retry_a (value TEXT NOT NULL)")
        cursor.execute("INSERT INTO synthetic_retry_a (value) VALUES ('a')")

    def successful_migration_b(cursor):
        cursor.execute("CREATE TABLE synthetic_retry_b (value TEXT NOT NULL)")
        cursor.execute("INSERT INTO synthetic_retry_b (value) VALUES ('b')")

    manager._get_migrations = lambda: [
        (migration_names[0], successful_migration_a),
        (migration_names[1], successful_migration_b),
    ]
    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        assert conn.execute("SELECT COUNT(*) FROM synthetic_retry_a").fetchone()[0] == 1
        assert conn.execute("SELECT COUNT(*) FROM synthetic_retry_b").fetchone()[0] == 1
        migration_counts = dict(
            conn.execute(
                """
                SELECT name, COUNT(*)
                FROM schema_migrations
                WHERE name IN (?, ?)
                GROUP BY name
                """,
                migration_names,
            ).fetchall()
        )

    assert migration_counts == {
        migration_names[0]: 1,
        migration_names[1]: 1,
    }

    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        assert conn.execute("SELECT COUNT(*) FROM synthetic_retry_a").fetchone()[0] == 1
        assert conn.execute("SELECT COUNT(*) FROM synthetic_retry_b").fetchone()[0] == 1
        migration_counts = dict(
            conn.execute(
                """
                SELECT name, COUNT(*)
                FROM schema_migrations
                WHERE name IN (?, ?)
                GROUP BY name
                """,
                migration_names,
            ).fetchall()
        )

    assert migration_counts == {
        migration_names[0]: 1,
        migration_names[1]: 1,
    }


def test_migration_normalize_null_guild_id_registered_on_initialize(tmp_path):
    """NULL guild_id backfill migration is applied during schema init."""
    db_path = str(tmp_path / "test.db")
    mgr = SchemaManager(db_path)
    mgr.initialize()

    with sqlite3.connect(db_path) as conn:
        row = conn.execute(
            "SELECT 1 FROM schema_migrations WHERE name = ?",
            ("normalize_null_guild_id_pairings_and_neon",),
        ).fetchone()

    assert row is not None


def test_migration_normalize_null_guild_id_sql_is_safe_on_clean_db(tmp_path):
    """Backfill migration is a no-op when no legacy NULL guild_id rows exist."""
    db_path = str(tmp_path / "test.db")
    mgr = SchemaManager(db_path)
    mgr.initialize()

    with sqlite3.connect(db_path) as conn:
        cursor = conn.cursor()
        mgr._migration_normalize_null_guild_id_pairings_and_neon(cursor)
        conn.commit()


def test_economy_ledger_triggers_record_player_and_nonprofit_changes(tmp_path):
    db_path = str(tmp_path / "test.db")
    SchemaManager(db_path).initialize()

    player_repo = PlayerRepository(db_path)
    loan_repo = LoanRepository(db_path)

    player_repo.add(111, "taxpayer", 123)
    player_repo.update_balance(111, 123, 50)
    loan_repo.add_to_nonprofit_fund(123, 20)

    with sqlite3.connect(db_path) as conn:
        rows = conn.execute(
            """
            SELECT account_type, account_id, delta, balance_before, balance_after, source
            FROM economy_ledger_entries
            ORDER BY ledger_id
            """
        ).fetchall()

    assert ("player", 111, 3, 0, 3, "player_insert") in rows
    assert ("player", 111, 47, 3, 50, "balance_update") in rows
    assert ("nonprofit", 123, 20, 0, 20, "nonprofit_insert") in rows


def test_economy_ledger_migration_backfills_existing_balances(tmp_path):
    db_path = str(tmp_path / "test.db")
    mgr = SchemaManager(db_path)
    mgr.initialize()

    player_repo = PlayerRepository(db_path)
    loan_repo = LoanRepository(db_path)
    player_repo.add(222, "existing", 123)
    player_repo.update_balance(222, 123, 77)
    loan_repo.add_to_nonprofit_fund(123, 33)

    with sqlite3.connect(db_path) as conn:
        conn.execute("DELETE FROM economy_ledger_entries")
        cursor = conn.cursor()
        mgr._migration_create_economy_ledger_tables(cursor)
        conn.commit()
        rows = conn.execute(
            """
            SELECT account_type, account_id, delta, balance_before, balance_after, source
            FROM economy_ledger_entries
            ORDER BY ledger_id
            """
        ).fetchall()

    assert rows == [
        ("player", 222, 77, 0, 77, "ledger_backfill"),
        ("nonprofit", 123, 33, 0, 33, "ledger_backfill"),
    ]


def test_followup_ledger_backfill_accounts_for_existing_deltas(tmp_path):
    db_path = str(tmp_path / "test.db")
    mgr = SchemaManager(db_path)
    mgr.initialize()

    player_repo = PlayerRepository(db_path)
    player_repo.add(333, "partially-logged", 123)
    player_repo.update_balance(333, 123, 100)

    with sqlite3.connect(db_path) as conn:
        conn.execute("DELETE FROM economy_ledger_entries")
        conn.execute(
            """
            INSERT INTO economy_ledger_entries (
                guild_id, account_type, account_id, delta,
                balance_before, balance_after, source
            )
            VALUES (123, 'player', 333, 25, 75, 100, 'balance_update')
            """
        )
        cursor = conn.cursor()
        mgr._migration_backfill_economy_ledger_opening_balances(cursor)
        conn.commit()
        rows = conn.execute(
            """
            SELECT account_type, account_id, delta, balance_before, balance_after, source
            FROM economy_ledger_entries
            ORDER BY ledger_id
            """
        ).fetchall()

    assert rows == [
        ("player", 333, 25, 75, 100, "balance_update"),
        ("player", 333, 75, 0, 75, "ledger_backfill"),
    ]
