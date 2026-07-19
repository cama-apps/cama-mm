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
    assert {"wheel_wars", "war_bets", "protected_hero_purchases"}.isdisjoint(tables)


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


def test_schema_manager_drops_retired_protected_hero_table(tmp_path):
    db_path = str(tmp_path / "legacy-protected-heroes.db")
    manager = SchemaManager(db_path)
    with sqlite3.connect(db_path) as conn:
        manager._migration_create_protected_hero_purchases_table(conn.cursor())

    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        remaining = conn.execute(
            """
            SELECT name
            FROM sqlite_master
            WHERE type = 'table' AND name = 'protected_hero_purchases'
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


def test_soft_avoid_duration_migration_caps_legacy_stacks(tmp_path):
    db_path = str(tmp_path / "legacy-soft-avoid-stack.db")
    manager = SchemaManager(db_path)
    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        conn.execute(
            """
            INSERT INTO soft_avoids
                (guild_id, avoider_discord_id, avoided_discord_id, games_remaining, created_at, updated_at)
            VALUES (123, 100, 200, 25, 1, 1)
            """
        )
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            ("cap_soft_avoid_games_remaining",),
        )

    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        games_remaining = conn.execute(
            """
            SELECT games_remaining
            FROM soft_avoids
            WHERE guild_id = 123 AND avoider_discord_id = 100 AND avoided_discord_id = 200
            """
        ).fetchone()[0]

    assert games_remaining == 10


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


def test_tunnels_columns_stay_in_sync_with_dig_update_whitelist(tmp_path):
    """Every tunnels column must be update_tunnel-writable or explicitly excluded.

    Pins the known failure class where a migration adds a tunnels column
    without adding it to DigRepository._TUNNEL_UPDATABLE_COLUMNS: the very
    first update_tunnel(...) touching the new column raises ValueError at
    runtime and breaks all digs. Adding a tunnels column requires updating
    the whitelist (and _TUNNEL_INT_COLS if integer-typed), or listing it in
    the exclusion set below with a reason.
    """
    from repositories.dig_repository import DigRepository

    db_path = str(tmp_path / "test.db")
    SchemaManager(db_path).initialize()

    with sqlite3.connect(db_path) as conn:
        columns = {row[1] for row in conn.execute("PRAGMA table_info(tunnels)")}
    assert columns, "tunnels table missing from initialized schema"

    # Columns update_tunnel legitimately never writes:
    excluded = {
        "discord_id",  # composite PK half; only used in the UPDATE WHERE clause
        "guild_id",  # composite PK half; only used in the UPDATE WHERE clause
        "created_at",  # set once by create_tunnel's INSERT, never mutated
        "retreat_cooldown_until",  # known-dormant column (no live writer)
        "engine_mode",  # known-dormant column (no live writer)
    }

    whitelist = DigRepository._TUNNEL_UPDATABLE_COLUMNS
    unaccounted = columns - whitelist - excluded
    assert not unaccounted, (
        f"tunnels columns missing from DigRepository._TUNNEL_UPDATABLE_COLUMNS: "
        f"{sorted(unaccounted)}. Add them to the whitelist (and _TUNNEL_INT_COLS "
        f"if integer-typed), or to this test's exclusion set with a reason."
    )

    # Reverse direction: a whitelisted or int-cast column with no migration
    # would also fail at runtime (SQLite error on UPDATE / bogus int cast).
    assert whitelist <= columns, (
        f"whitelisted columns missing from tunnels table: {sorted(whitelist - columns)}"
    )
    assert columns >= DigRepository._TUNNEL_INT_COLS, (
        f"_TUNNEL_INT_COLS entries missing from tunnels table: "
        f"{sorted(DigRepository._TUNNEL_INT_COLS - columns)}"
    )
