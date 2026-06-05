import sqlite3

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
