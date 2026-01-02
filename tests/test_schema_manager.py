import os
import sqlite3
import tempfile

from infrastructure.schema_manager import SchemaManager


def test_schema_manager_initializes_tables():
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
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
            "bets",
            "pending_matches",
            "lobby_state",
            "schema_migrations",
        }
        assert required.issubset(tables)
    finally:
        try:
            os.unlink(db_path)
        except OSError:
            pass

