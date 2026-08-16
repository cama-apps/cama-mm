import json
import sqlite3

import pytest

from infrastructure.schema_manager import SchemaManager
from rating_system import CamaRatingSystem
from repositories.loan_repository import LoanRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID


def test_schema_manager_initializes_tables(repo_db_path):
    """The fully migrated schema has all required tables, region columns, and
    the reminder-preference column/index.

    Uses the session schema template (built by a real ``initialize()``) instead
    of re-running all migrations. Merged from
    test_schema_manager_adds_region_columns and
    test_schema_manager_adds_lobby_reminder_preference.
    """
    with sqlite3.connect(repo_db_path) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
        tables = {row[0] for row in cursor.fetchall()}

        player_columns = {row[1] for row in conn.execute("PRAGMA table_info(players)")}

        reminder_columns = {
            row[1] for row in conn.execute("PRAGMA table_info(reminder_preferences)")
        }
        reminder_indexes = {
            row[1] for row in conn.execute("PRAGMA index_list(reminder_preferences)")
        }
        conn.execute(
            """
            INSERT INTO reminder_preferences (discord_id, guild_id)
            VALUES (?, ?)
            """,
            (123, TEST_GUILD_ID),
        )
        lobby_enabled = conn.execute(
            """
            SELECT lobby_enabled
            FROM reminder_preferences
            WHERE discord_id = ? AND guild_id = ?
            """,
            (123, TEST_GUILD_ID),
        ).fetchone()[0]

    required = {
        "players",
        "matches",
        "match_participants",
        "rating_history",
        "match_predictions",
        "bets",
        "pending_matches",
        "draft_finalization_jobs",
        "lobby_state",
        "lobby_target_subscriptions",
        "schema_migrations",
        "economy_ledger_entries",
        "economy_ledger_context",
        "wrapped_enrichment_facts",
    }
    assert required.issubset(tables)
    assert {"wheel_wars", "war_bets", "protected_hero_purchases"}.isdisjoint(tables)

    assert {"preferred_region", "inferred_region"}.issubset(player_columns)

    assert "lobby_enabled" in reminder_columns
    assert "idx_reminder_prefs_lobby" in reminder_indexes
    assert lobby_enabled == 0


def test_draft_finalization_jobs_migration_is_additive_and_constrained(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        columns = {
            row[1]: (row[2], row[3])
            for row in conn.execute("PRAGMA table_info(draft_finalization_jobs)")
        }
        indexes = {
            row[1]
            for row in conn.execute("PRAGMA index_list(draft_finalization_jobs)")
        }
        migration = conn.execute(
            "SELECT 1 FROM schema_migrations WHERE name='create_draft_finalization_jobs'"
        ).fetchone()
        conn.execute(
            "INSERT INTO pending_matches(guild_id,payload,completion_key) VALUES (42,'{}','draft:42:1')"
        )
        pending_match_id = conn.execute(
            "SELECT pending_match_id FROM pending_matches WHERE completion_key='draft:42:1'"
        ).fetchone()[0]
        conn.execute(
            """
            INSERT INTO draft_finalization_jobs(
                completion_key,guild_id,session_id,pending_match_id,stage,plan_json
            ) VALUES (?,?,?,?,?,?)
            """,
            ("draft:42:1", 42, 1, pending_match_id, "linked", '{"future":true}'),
        )
        progress_json = conn.execute(
            "SELECT progress_json FROM draft_finalization_jobs WHERE completion_key='draft:42:1'"
        ).fetchone()[0]

        with pytest.raises(sqlite3.IntegrityError):
            conn.execute(
                """
                INSERT INTO draft_finalization_jobs(
                    completion_key,guild_id,session_id,pending_match_id,stage,plan_json
                ) VALUES ('draft:42:2',42,2,999,'linked','[]')
                """
            )
        with pytest.raises(sqlite3.IntegrityError):
            conn.execute(
                """
                INSERT INTO draft_finalization_jobs(
                    completion_key,guild_id,session_id,pending_match_id,stage,plan_json
                ) VALUES (NULL,43,3,999,'linked','{}')
                """
            )

    assert {
        "completion_key",
        "guild_id",
        "session_id",
        "pending_match_id",
        "revision",
        "stage",
        "plan_json",
        "progress_json",
        "lease_owner",
        "lease_until",
        "last_error",
        "created_at",
        "updated_at",
    } == set(columns)
    assert columns["completion_key"] == ("TEXT", 1)
    assert columns["plan_json"] == ("TEXT", 1)
    assert columns["progress_json"] == ("TEXT", 1)
    assert "idx_draft_finalization_jobs_incomplete" in indexes
    assert migration == (1,)
    assert progress_json == "{}"


def test_draft_financial_effects_migration_is_additive_and_constrained(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        columns = {
            row[1]: (row[2], row[3])
            for row in conn.execute("PRAGMA table_info(draft_financial_effects)")
        }
        indexes = {
            row[1]
            for row in conn.execute("PRAGMA index_list(draft_financial_effects)")
        }
        migration = conn.execute(
            "SELECT 1 FROM schema_migrations "
            "WHERE name='create_draft_financial_effects'"
        ).fetchone()
        conn.execute(
            "INSERT INTO pending_matches(guild_id,payload,completion_key) "
            "VALUES (42,'{}','draft:42:1')"
        )
        pending_match_id = conn.execute(
            "SELECT pending_match_id FROM pending_matches "
            "WHERE completion_key='draft:42:1'"
        ).fetchone()[0]
        conn.execute(
            """
            INSERT INTO draft_finalization_jobs(
                completion_key,guild_id,session_id,pending_match_id,stage,plan_json
            ) VALUES (?,?,?,?,?,?)
            """,
            ("draft:42:1", 42, 1, pending_match_id, "linked", '{"schema_version":1}'),
        )
        conn.execute(
            """
            INSERT INTO draft_financial_effects(
                effect_key,completion_key,guild_id,session_id,pending_match_id,
                effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
            """,
            (
                "draft:42:1:seed:1",
                "draft:42:1",
                42,
                1,
                pending_match_id,
                "seed",
                0,
                "0" * 64,
                '{"reserved":0}',
                "applied",
                '{"reserved":0}',
            ),
        )
        with pytest.raises(sqlite3.IntegrityError):
            conn.execute(
                """
                INSERT INTO draft_financial_effects(
                    effect_key,completion_key,guild_id,session_id,pending_match_id,
                    effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json
                ) VALUES ('bad','draft:42:1',42,1,?,'blind',1,?,'[]','applied','{}')
                """,
                (pending_match_id, "0" * 64),
            )

    assert {
        "effect_key",
        "completion_key",
        "guild_id",
        "session_id",
        "pending_match_id",
        "effect_kind",
        "ordinal",
        "plan_sha256",
        "intended_json",
        "status",
        "receipt_json",
        "created_at",
        "updated_at",
    } == set(columns)
    assert columns["effect_key"] == ("TEXT", 1)
    assert columns["intended_json"] == ("TEXT", 1)
    assert columns["receipt_json"] == ("TEXT", 1)
    assert "idx_draft_financial_effects_completion" in indexes
    assert migration == (1,)


def test_schema_manager_drops_retired_tables(tmp_path):
    """One initialize drops every retired legacy table.

    Merged from test_schema_manager_drops_retired_wheel_war_tables,
    test_schema_manager_drops_retired_protected_hero_table, and
    test_schema_manager_drops_retired_curses_table — one initialize covers
    all three drop migrations.
    """
    db_path = str(tmp_path / "legacy-retired-tables.db")
    manager = SchemaManager(db_path)
    with sqlite3.connect(db_path) as conn:
        conn.execute("CREATE TABLE wheel_wars (war_id INTEGER PRIMARY KEY)")
        conn.execute("CREATE TABLE war_bets (bet_id INTEGER PRIMARY KEY)")
        manager._migration_create_protected_hero_purchases_table(conn.cursor())
        manager._migration_create_curses_table(conn.cursor())

    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        remaining = conn.execute(
            """
            SELECT name
            FROM sqlite_master
            WHERE type = 'table' AND name IN
                ('wheel_wars', 'war_bets', 'protected_hero_purchases', 'curses')
            """
        ).fetchall()

    assert remaining == []


def test_schema_manager_adds_lobby_target_subscription_table_and_index(repo_db_path):
    """One-shot player watches have their own target-keyed unique index."""
    db_path = repo_db_path

    with sqlite3.connect(db_path) as conn:
        columns = conn.execute(
            "PRAGMA table_info(lobby_target_subscriptions)"
        ).fetchall()
        indexes = conn.execute(
            "PRAGMA index_list(lobby_target_subscriptions)"
        ).fetchall()

        assert [row[1] for row in columns] == [
            "guild_id",
            "target_id",
            "subscriber_id",
            "created_at",
        ]
        assert {row[1]: row[5] for row in columns} == {
            "guild_id": 1,
            "target_id": 2,
            "subscriber_id": 3,
            "created_at": 0,
        }

        # SQLite materializes the composite primary key as a unique index.
        # Its guild/target prefix directly backs the atomic claim lookup.
        primary_key_index = next(row for row in indexes if row[3] == "pk")
        assert primary_key_index[2] == 1
        indexed_columns = conn.execute(
            f'PRAGMA index_info("{primary_key_index[1]}")'
        ).fetchall()
        assert [row[2] for row in indexed_columns] == [
            "guild_id",
            "target_id",
            "subscriber_id",
        ]


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


def test_streak_rate_migration_backfills_legacy_history(tmp_path):
    """Pre-rate rating rows retain the historical 20% correction curve."""
    db_path = str(tmp_path / "legacy-streak-rate.db")
    manager = SchemaManager(db_path)

    with sqlite3.connect(db_path) as conn:
        conn.row_factory = sqlite3.Row
        conn.execute(
            """
            CREATE TABLE rating_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                streak_length INTEGER,
                streak_multiplier REAL
            )
            """
        )
        conn.execute(
            """
            INSERT INTO rating_history (streak_length, streak_multiplier)
            VALUES (4, 1.40)
            """
        )

        migration = getattr(
            manager,
            "_migration_add_streak_multiplier_per_game_to_rating_history",
            None,
        )
        assert migration is not None
        migration(conn.cursor())

        stored_rate = conn.execute(
            "SELECT streak_multiplier_per_game FROM rating_history"
        ).fetchone()[0]

    assert stored_rate == pytest.approx(0.20)


def test_base_delta_multiplier_migration_backfills_legacy_history(tmp_path):
    """Pre-multiplier rating rows retain the historical 0.75 calibration."""
    db_path = str(tmp_path / "legacy-base-delta-multiplier.db")
    manager = SchemaManager(db_path)

    with sqlite3.connect(db_path) as conn:
        conn.row_factory = sqlite3.Row
        conn.execute(
            """
            CREATE TABLE rating_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rating REAL
            )
            """
        )
        conn.execute("INSERT INTO rating_history (rating) VALUES (1510.0)")

        migration = getattr(
            manager,
            "_migration_add_base_rating_delta_multiplier_to_rating_history",
            None,
        )
        assert migration is not None
        migration(conn.cursor())

        stored_multiplier = conn.execute(
            "SELECT base_rating_delta_multiplier FROM rating_history"
        ).fetchone()[0]

    assert stored_multiplier == pytest.approx(0.75)


def test_low_priority_gain_migration_keeps_legacy_history_unboosted(tmp_path):
    db_path = str(tmp_path / "legacy-low-priority-gain.db")
    manager = SchemaManager(db_path)

    with sqlite3.connect(db_path) as conn:
        conn.row_factory = sqlite3.Row
        conn.execute(
            """
            CREATE TABLE rating_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rating REAL
            )
            """
        )
        conn.execute("INSERT INTO rating_history (rating) VALUES (1510.0)")

        manager._migration_add_low_priority_gain_multiplier_to_rating_history(
            conn.cursor()
        )
        stored_multiplier = conn.execute(
            "SELECT low_priority_gain_multiplier FROM rating_history"
        ).fetchone()[0]

    assert stored_multiplier == pytest.approx(1.0)


def test_wrapped_enrichment_facts_migration_backfills_safely_and_idempotently(
    repo_db_path,
):
    """Backfill valid payloads once, preserve unmatched semantics, skip corruption."""
    with sqlite3.connect(repo_db_path) as conn:
        conn.executemany(
            """
            INSERT INTO players (
                discord_id, guild_id, discord_username, steam_id
            ) VALUES (?, ?, ?, ?)
            """,
            [
                (100, TEST_GUILD_ID, "matched", 99999),
                (200, TEST_GUILD_ID, "unmatched", None),
                (300, TEST_GUILD_ID, "malformed", None),
            ],
        )
        # The junction ID takes precedence over the stale legacy players.steam_id.
        conn.execute(
            """
            INSERT INTO player_steam_ids (
                discord_id, steam_id, is_primary, added_at
            ) VALUES (?, ?, 1, 1)
            """,
            (100, 12345),
        )

    repo = MatchRepository(repo_db_path)
    valid_match_id = repo.record_match(
        [100],
        [200],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    malformed_match_id = repo.record_match(
        [300],
        [],
        winning_team=1,
        guild_id=TEST_GUILD_ID,
    )
    payload = {
        "players": [
            {
                "account_id": 12345,
                "actions_per_min": 321.5,
                "courier_kills": 2,
                "pings": 44,
                "lane_role": 2,
                "purchase_log": [
                    {"key": "rapier"},
                    {"key": "ward_observer"},
                    {"key": "rapier"},
                ],
            }
        ],
        "comeback": 9000,
        "throw": 4000,
    }
    with sqlite3.connect(repo_db_path) as conn:
        conn.execute(
            "UPDATE matches SET enrichment_data = ? WHERE match_id = ?",
            (json.dumps(payload), valid_match_id),
        )
        conn.execute(
            "UPDATE matches SET enrichment_data = ? WHERE match_id = ?",
            ("{not-json", malformed_match_id),
        )
        # Recreate the pre-migration state so initialize exercises the real
        # schema-ledger path, not merely the migration helper in isolation.
        conn.execute("DROP TABLE wrapped_enrichment_facts")
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            ("create_wrapped_enrichment_facts",),
        )

    manager = SchemaManager(repo_db_path)
    manager.initialize()

    def load_facts():
        with sqlite3.connect(repo_db_path) as conn:
            return conn.execute(
                """
                SELECT
                    discord_id, actions_per_min, courier_kills, pings,
                    rapier_count, lane_role, comeback, throw
                FROM wrapped_enrichment_facts
                WHERE guild_id = ? AND match_id = ?
                ORDER BY discord_id
                """,
                (TEST_GUILD_ID, valid_match_id),
            ).fetchall()

    expected = [
        (100, 321.5, 2, 44, 2, 2, 9000, 4000),
        (200, None, None, None, 0, None, 9000, 4000),
    ]
    assert load_facts() == expected
    with sqlite3.connect(repo_db_path) as conn:
        assert conn.execute(
            """
            SELECT COUNT(*)
            FROM wrapped_enrichment_facts
            WHERE match_id = ?
            """,
            (malformed_match_id,),
        ).fetchone()[0] == 0

        # A repeated migration rebuilds the projection instead of duplicating
        # or preserving stale derived values.
        conn.execute(
            """
            UPDATE wrapped_enrichment_facts
            SET actions_per_min = -1
            WHERE guild_id = ? AND match_id = ? AND discord_id = 100
            """,
            (TEST_GUILD_ID, valid_match_id),
        )
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            ("create_wrapped_enrichment_facts",),
        )

    manager.initialize()
    assert load_facts() == expected


def test_prediction_probability_migration_recomputes_history_symmetrically(repo_db_path):
    manager = SchemaManager(repo_db_path)
    with sqlite3.connect(repo_db_path) as conn:
        cursor = conn.cursor()
        cursor.execute(
            """
            INSERT INTO match_predictions (
                match_id, radiant_rating, dire_rating, radiant_rd, dire_rd,
                expected_radiant_win_prob
            ) VALUES (?, ?, ?, ?, ?, ?)
            """,
            (9876, 1700.0, 1500.0, 350.0, 50.0, 0.7571404149989154),
        )
        cursor.executemany(
            """
            INSERT INTO rating_history (
                discord_id, match_id, team_number, expected_team_win_prob
            ) VALUES (?, ?, ?, ?)
            """,
            [
                (1, 9876, 1, 0.7571404149989154),
                (2, 9876, 2, 0.31641538274428405),
            ],
        )

        manager._migration_recompute_glicko_prediction_probabilities(cursor)

        stored_prediction = cursor.execute(
            """
            SELECT expected_radiant_win_prob
            FROM match_predictions
            WHERE match_id = 9876
            """
        ).fetchone()[0]
        stored_history = cursor.execute(
            """
            SELECT team_number, expected_team_win_prob
            FROM rating_history
            WHERE match_id = 9876
            ORDER BY team_number
            """
        ).fetchall()

    expected = CamaRatingSystem.predict_win_probability(
        1700.0, 350.0, 1500.0, 50.0
    )
    assert stored_prediction == pytest.approx(expected)
    assert stored_history == pytest.approx([(1, expected), (2, 1.0 - expected)])


def test_duration_migrations_cap_legacy_stacks(repo_db_path):
    """The soft-avoid and package-deal cap migrations clamp legacy stacks to 10.

    Merged from test_soft_avoid_duration_migration_caps_legacy_stacks and
    test_package_deal_duration_migration_caps_legacy_stacks — one re-run of
    initialize over the session schema template covers both.
    """
    db_path = repo_db_path
    manager = SchemaManager(db_path)

    with sqlite3.connect(db_path) as conn:
        conn.execute(
            """
            INSERT INTO soft_avoids
                (guild_id, avoider_discord_id, avoided_discord_id, games_remaining, created_at, updated_at)
            VALUES (123, 100, 200, 25, 1, 1)
            """
        )
        conn.execute("DROP TRIGGER trg_package_deals_games_remaining_insert_cap")
        conn.execute("DROP TRIGGER trg_package_deals_games_remaining_update_cap")
        conn.execute(
            """
            INSERT INTO package_deals
                (guild_id, buyer_discord_id, partner_discord_id, games_remaining, cost_paid, created_at, updated_at)
            VALUES (123, 100, 200, 25, 500, 1, 1)
            """
        )
        conn.execute(
            "DELETE FROM schema_migrations WHERE name IN (?, ?)",
            ("cap_soft_avoid_games_remaining", "cap_package_deal_games_remaining"),
        )

    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        avoid_games_remaining = conn.execute(
            """
            SELECT games_remaining
            FROM soft_avoids
            WHERE guild_id = 123 AND avoider_discord_id = 100 AND avoided_discord_id = 200
            """
        ).fetchone()[0]
        deal_games_remaining = conn.execute(
            """
            SELECT games_remaining
            FROM package_deals
            WHERE guild_id = 123 AND buyer_discord_id = 100 AND partner_discord_id = 200
            """
        ).fetchone()[0]

    assert avoid_games_remaining == 10
    assert deal_games_remaining == 10


def test_package_deal_duration_cap_is_enforced_after_migrations(repo_db_path):
    db_path = repo_db_path

    with sqlite3.connect(db_path) as conn:
        conn.execute(
            """
            INSERT INTO package_deals
                (guild_id, buyer_discord_id, partner_discord_id, games_remaining,
                 cost_paid, created_at, updated_at)
            VALUES (123, 100, 200, 10, 500, 1, 1)
            """
        )

        with pytest.raises(sqlite3.IntegrityError):
            conn.execute(
                """
                INSERT INTO package_deals
                    (guild_id, buyer_discord_id, partner_discord_id, games_remaining,
                     cost_paid, created_at, updated_at)
                VALUES (123, 300, 400, 11, 500, 1, 1)
                """
            )

        with pytest.raises(sqlite3.IntegrityError):
            conn.execute(
                """
                UPDATE package_deals
                SET games_remaining = 11
                WHERE guild_id = 123
                  AND buyer_discord_id = 100
                  AND partner_discord_id = 200
                """
            )


def test_failed_pending_batch_rolls_back_and_retries_cleanly(tmp_path):
    """A failed migration batch rolls back all schema and ledger rows, and a
    later retry applies cleanly and idempotently.

    Merged from test_failed_pending_batch_rolls_back_all_schema_and_migration_rows
    and test_failed_pending_batch_retries_cleanly — the retry test starts from
    the exact rolled-back state the first test asserted, so both live in one
    fail → verify-rollback → retry → verify flow.
    """
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

    # The whole batch — tables AND migration-ledger rows — was rolled back.
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


def test_migration_normalize_null_guild_id_registered_and_safe_on_clean_db(
    repo_db_path,
):
    """NULL guild_id backfill migration is applied during schema init and is a
    no-op when no legacy NULL guild_id rows exist.

    Merged from test_migration_normalize_null_guild_id_registered_on_initialize
    and test_migration_normalize_null_guild_id_sql_is_safe_on_clean_db; both
    read from the session schema template built by a real initialize().
    """
    db_path = repo_db_path
    mgr = SchemaManager(db_path)

    with sqlite3.connect(db_path) as conn:
        row = conn.execute(
            "SELECT 1 FROM schema_migrations WHERE name = ?",
            ("normalize_null_guild_id_pairings_and_neon",),
        ).fetchone()
        assert row is not None

        # Re-running the backfill helper on a clean db must not raise.
        cursor = conn.cursor()
        mgr._migration_normalize_null_guild_id_pairings_and_neon(cursor)
        conn.commit()


def test_economy_ledger_triggers_record_player_and_nonprofit_changes(repo_db_path):
    db_path = repo_db_path

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


def test_economy_ledger_migration_backfills_existing_balances(repo_db_path):
    db_path = repo_db_path
    mgr = SchemaManager(db_path)

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


def test_economy_event_severity_migration_preserves_history_and_allows_level_five(
    repo_db_path,
):
    db_path = repo_db_path
    manager = SchemaManager(db_path)

    with sqlite3.connect(db_path) as conn:
        conn.execute(
            """
            INSERT INTO economy_daily_events (
                guild_id, event_date, name, hero, direction, severity,
                target_effect_jc, forecast_flow_jc, expected_effect_jc,
                monetary_stock_before, effects, announcement,
                starts_at, ends_at, created_at, announced_at
            )
            VALUES (
                123, '2026-07-28', 'Legacy Edict', 'Doom', 'deflationary', 3,
                -30, 40, -30, 1000, '{}', 'Legacy announcement',
                100, 200, 90, 110
            )
            """
        )
        conn.execute("DROP INDEX idx_economy_events_active")
        conn.execute(
            "ALTER TABLE economy_daily_events RENAME TO economy_daily_events_level_five"
        )
        current_sql = conn.execute(
            """
            SELECT sql
            FROM sqlite_master
            WHERE type = 'table' AND name = 'economy_daily_events_level_five'
            """
        ).fetchone()[0]
        legacy_sql = current_sql.replace(
            "economy_daily_events_level_five", "economy_daily_events"
        ).replace("BETWEEN 1 AND 5", "BETWEEN 1 AND 3")
        conn.execute(legacy_sql)
        columns = [
            row[1]
            for row in conn.execute("PRAGMA table_info(economy_daily_events_level_five)")
        ]
        column_list = ", ".join(columns)
        conn.execute(
            f"""
            INSERT INTO economy_daily_events ({column_list})
            SELECT {column_list}
            FROM economy_daily_events_level_five
            """
        )
        conn.execute("DROP TABLE economy_daily_events_level_five")
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            ("expand_economy_event_severity_levels",),
        )

    manager.initialize()

    with sqlite3.connect(db_path) as conn:
        legacy = conn.execute(
            """
            SELECT event_id, name, severity, announced_at
            FROM economy_daily_events
            WHERE guild_id = 123
            """
        ).fetchone()
        conn.execute(
            """
            INSERT INTO economy_daily_events (
                guild_id, event_date, name, hero, direction, severity,
                target_effect_jc, forecast_flow_jc, expected_effect_jc,
                monetary_stock_before, effects, announcement,
                starts_at, ends_at, created_at
            )
            VALUES (
                123, '2026-07-29', 'Level Five Edict', 'Doom', 'deflationary', 5,
                -50, 60, -50, 1000, '{}', 'Level five announcement',
                200, 300, 190
            )
            """
        )

    assert legacy == (1, "Legacy Edict", 3, 110)


def test_followup_ledger_backfill_accounts_for_existing_deltas(repo_db_path):
    db_path = repo_db_path
    mgr = SchemaManager(db_path)

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


def test_tunnels_columns_stay_in_sync_with_dig_update_whitelist(repo_db_path):
    """Every tunnels column must be update_tunnel-writable or explicitly excluded.

    Pins the known failure class where a migration adds a tunnels column
    without adding it to DigRepository._TUNNEL_UPDATABLE_COLUMNS: the very
    first update_tunnel(...) touching the new column raises ValueError at
    runtime and breaks all digs. Adding a tunnels column requires updating
    the whitelist (and _TUNNEL_INT_COLS if integer-typed), or listing it in
    the exclusion set below with a reason.
    """
    from repositories.dig_repository import DigRepository

    db_path = repo_db_path

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
