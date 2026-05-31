import sqlite3

from infrastructure.schema_manager import SchemaManager


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
    }
    assert required.issubset(tables)


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
