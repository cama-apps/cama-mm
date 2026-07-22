"""Tests for dig-system schema migrations."""

import json
import sqlite3

from infrastructure.schema_manager import SchemaManager
from repositories.dig_repository import DigRepository


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


def _run_reconcile_stat_points_migration(db_path: str) -> None:
    """Invoke the post-prestige S-stat reconciliation directly."""
    manager = SchemaManager(db_path)
    conn = sqlite3.connect(db_path)
    try:
        cursor = conn.cursor()
        manager._migration_reconcile_post_prestige_boss_stat_points(cursor)
        conn.commit()
    finally:
        conn.close()


def _run_reconcile_cumulative_stat_points_migration(db_path: str) -> None:
    """Invoke the cumulative prestige S-stat reconciliation directly."""
    manager = SchemaManager(db_path)
    conn = sqlite3.connect(db_path)
    try:
        cursor = conn.cursor()
        manager._migration_reconcile_cumulative_prestige_boss_stat_points(cursor)
        conn.commit()
    finally:
        conn.close()


def _read_tunnel_stats(db_path: str, discord_id: int, guild_id: int) -> tuple[int, str]:
    conn = sqlite3.connect(db_path)
    try:
        row = conn.execute(
            "SELECT stat_points, stat_boss_awards FROM tunnels "
            "WHERE discord_id = ? AND guild_id = ?",
            (discord_id, guild_id),
        ).fetchone()
        return row[0], row[1]
    finally:
        conn.close()


class TestDigAutoBuySettingsMigration:
    def test_auto_buy_columns_default_and_update(self, repo_db_path):
        repo = DigRepository(repo_db_path)
        tunnel = repo.create_tunnel(10001, 12345, "Auto")

        assert tunnel["auto_buy_torch"] == 0
        assert tunnel["auto_buy_hard_hat"] == 0

        repo.update_tunnel(10001, 12345, auto_buy_torch=1, auto_buy_hard_hat=1)
        updated = repo.get_tunnel(10001, 12345)
        assert updated["auto_buy_torch"] == 1
        assert updated["auto_buy_hard_hat"] == 1


class TestDigActionHistoryIndexesMigration:
    def test_replaces_prefix_indexes_with_chronological_indexes(self, repo_db_path):
        manager = SchemaManager(repo_db_path)
        conn = sqlite3.connect(repo_db_path)
        try:
            manager._migration_add_dig_action_history_indexes(conn.cursor())
            manager._migration_add_dig_action_history_indexes(conn.cursor())
            conn.commit()

            indexes = {
                row[1]
                for row in conn.execute("PRAGMA index_list(dig_actions)").fetchall()
            }
            assert "idx_dig_actions_guild_actor" not in indexes
            assert "idx_dig_actions_guild_target" not in indexes

            expected = {
                "idx_dig_actions_guild_actor_created": [
                    "guild_id",
                    "actor_id",
                    "created_at",
                ],
                "idx_dig_actions_guild_target_created": [
                    "guild_id",
                    "target_id",
                    "created_at",
                ],
            }
            for index_name, columns in expected.items():
                assert index_name in indexes
                index_info = conn.execute(
                    f"PRAGMA index_xinfo({index_name})"
                ).fetchall()
                assert [row[2] for row in index_info if row[5]] == columns
                assert next(row[3] for row in index_info if row[2] == "created_at") == 1

            applied = {
                row[0]
                for row in conn.execute(
                    "SELECT name FROM schema_migrations"
                ).fetchall()
            }
            assert "add_dig_action_history_indexes" in applied
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


class TestReconcilePostPrestigeBossStatPoints:
    """Covers _migration_reconcile_post_prestige_boss_stat_points."""

    def _seed(
        self,
        db_path: str,
        *,
        discord_id: int = 111,
        guild_id: int = 222,
        prestige_level: int = 1,
        stat_points: int = 6,
        stat_boss_awards=None,
        boss_progress=None,
    ) -> None:
        awards = stat_boss_awards if stat_boss_awards is not None else []
        progress = boss_progress if boss_progress is not None else {}
        conn = sqlite3.connect(db_path)
        try:
            conn.execute(
                """
                INSERT INTO tunnels (
                    discord_id, guild_id, prestige_level, stat_points,
                    stat_boss_awards, boss_progress
                )
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    discord_id,
                    guild_id,
                    prestige_level,
                    stat_points,
                    json.dumps(awards) if not isinstance(awards, str) else awards,
                    json.dumps(progress) if not isinstance(progress, str) else progress,
                ),
            )
            conn.commit()
        finally:
            conn.close()

    def test_grants_missing_current_prestige_boss_awards(self, repo_db_path):
        self._seed(
            repo_db_path,
            stat_points=6,
            stat_boss_awards={"prestige_level": 1, "awards": [25]},
            boss_progress={
                "25": {"status": "defeated", "boss_id": "grothak"},
                "50": "defeated",
                "75": "phase1_defeated",
                "100": "active",
            },
        )

        _run_reconcile_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 7
        assert json.loads(awards_raw) == {
            "prestige_level": 1,
            "awards": [25, 50],
        }

    def test_legacy_global_awards_do_not_block_prestiged_reclear(self, repo_db_path):
        self._seed(
            repo_db_path,
            stat_points=6,
            stat_boss_awards=[25],
            boss_progress={"25": "defeated"},
        )

        _run_reconcile_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 7
        assert json.loads(awards_raw) == {
            "prestige_level": 1,
            "awards": [25],
        }

    def test_skips_point_grant_when_cumulative_total_already_correct(self, repo_db_path):
        """Legacy list-format awards must not inflate stat_points when total is already synced."""
        self._seed(
            repo_db_path,
            prestige_level=1,
            stat_points=19,
            stat_boss_awards=[25, 50, 75, 100, 150, 200, 275],
            boss_progress={
                "25": "defeated",
                "50": "defeated",
                "75": "defeated",
                "100": "defeated",
                "150": "defeated",
                "200": "defeated",
                "275": "defeated",
            },
        )

        _run_reconcile_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 19
        assert json.loads(awards_raw) == {
            "prestige_level": 1,
            "awards": [25, 50, 75, 100, 150, 200, 275],
        }

    def test_idempotent_after_reconciliation(self, repo_db_path):
        self._seed(
            repo_db_path,
            stat_points=5,
            stat_boss_awards={"prestige_level": 2, "awards": []},
            boss_progress={"25": "defeated", "50": "defeated"},
            prestige_level=2,
        )

        _run_reconcile_stat_points_migration(repo_db_path)
        _run_reconcile_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 7
        assert json.loads(awards_raw) == {
            "prestige_level": 2,
            "awards": [25, 50],
        }

    def test_skips_unprestiged_tunnels(self, repo_db_path):
        self._seed(
            repo_db_path,
            prestige_level=0,
            stat_points=5,
            stat_boss_awards=[],
            boss_progress={"25": "defeated"},
        )

        _run_reconcile_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 5
        assert json.loads(awards_raw) == []

    def test_runs_during_normal_initialization(self, tmp_path):
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
        assert "reconcile_post_prestige_boss_stat_points" in applied


class TestReconcileCumulativePrestigeBossStatPoints:
    """Covers _migration_reconcile_cumulative_prestige_boss_stat_points."""

    def _seed(
        self,
        db_path: str,
        *,
        discord_id: int = 111,
        guild_id: int = 222,
        prestige_level: int = 1,
        stat_points: int = 12,
        stat_boss_awards=None,
        boss_progress=None,
    ) -> None:
        awards = stat_boss_awards if stat_boss_awards is not None else {
            "prestige_level": prestige_level,
            "awards": [],
        }
        progress = boss_progress if boss_progress is not None else {}
        conn = sqlite3.connect(db_path)
        try:
            conn.execute(
                """
                INSERT INTO tunnels (
                    discord_id, guild_id, prestige_level, stat_points,
                    stat_boss_awards, boss_progress
                )
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    discord_id,
                    guild_id,
                    prestige_level,
                    stat_points,
                    json.dumps(awards),
                    json.dumps(progress),
                ),
            )
            conn.commit()
        finally:
            conn.close()

    def test_completed_prestige_levels_imply_prior_tier_boss_points(self, repo_db_path):
        self._seed(
            repo_db_path,
            prestige_level=5,
            stat_points=16,
            stat_boss_awards={"prestige_level": 5, "awards": [25, 50, 75, 100]},
            boss_progress={
                "25": "defeated",
                "50": "defeated",
                "75": "defeated",
                "100": "defeated",
                "150": "active",
            },
        )

        _run_reconcile_cumulative_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 44  # 5 base + 5 completed prestiges * 7 + 4 current
        assert json.loads(awards_raw) == {
            "prestige_level": 5,
            "awards": [25, 50, 75, 100],
        }

    def test_current_run_defeated_bosses_are_folded_into_awards(self, repo_db_path):
        self._seed(
            repo_db_path,
            prestige_level=2,
            stat_points=13,
            stat_boss_awards={"prestige_level": 2, "awards": []},
            boss_progress={"25": {"status": "defeated", "boss_id": "grothak"}},
        )

        _run_reconcile_cumulative_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 20  # 5 base + 2 completed prestiges * 7 + 1 current
        assert json.loads(awards_raw) == {
            "prestige_level": 2,
            "awards": [25],
        }

    def test_does_not_lower_already_higher_totals(self, repo_db_path):
        self._seed(
            repo_db_path,
            prestige_level=1,
            stat_points=99,
            stat_boss_awards={"prestige_level": 1, "awards": [25]},
            boss_progress={"25": "defeated"},
        )

        _run_reconcile_cumulative_stat_points_migration(repo_db_path)

        stat_points, awards_raw = _read_tunnel_stats(repo_db_path, 111, 222)
        assert stat_points == 99
        assert json.loads(awards_raw) == {
            "prestige_level": 1,
            "awards": [25],
        }

    def test_runs_during_normal_initialization(self, tmp_path):
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
        assert "reconcile_cumulative_prestige_boss_stat_points" in applied


class TestDigGearMigration:
    """Covers _migration_create_dig_gear_system: schema + backfill."""

    def _rerun_gear_migration(self, db_path: str) -> None:
        """Re-execute the gear migration directly on an already-initialized DB.

        Used to simulate the upgrade case where pre-existing tunnels need
        a Weapon row backfilled.
        """
        manager = SchemaManager(db_path)
        conn = sqlite3.connect(db_path)
        try:
            cursor = conn.cursor()
            # Pretend it never ran so we can replay it
            cursor.execute(
                "DELETE FROM schema_migrations WHERE name = 'create_dig_gear_system'"
            )
            cursor.execute("DROP TABLE IF EXISTS dig_gear")
            manager._migration_create_dig_gear_system(cursor)
            conn.commit()
        finally:
            conn.close()

    def test_fresh_init_creates_table_and_indexes(self, tmp_path):
        db_path = str(tmp_path / "fresh.db")
        SchemaManager(db_path).initialize()
        conn = sqlite3.connect(db_path)
        try:
            cols = {row[1] for row in conn.execute("PRAGMA table_info(dig_gear)").fetchall()}
            indexes = {
                row[0]
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='dig_gear'"
                ).fetchall()
            }
        finally:
            conn.close()
        assert {"id", "discord_id", "guild_id", "slot", "tier",
                "durability", "equipped", "acquired_at", "source"} <= cols
        assert "idx_dig_gear_player_slot" in indexes
        assert "uq_dig_gear_one_equipped_per_slot" in indexes

    def test_fresh_init_registers_migration(self, tmp_path):
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
        assert "create_dig_gear_system" in applied

    def test_backfills_weapon_for_each_existing_tunnel(self, repo_db_path):
        """After migration, every tunnel has exactly one equipped Weapon row."""
        # Seed a few tunnels with various pickaxe tiers and last_dig_at values
        conn = sqlite3.connect(repo_db_path)
        try:
            conn.execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, pickaxe_tier, last_dig_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (1, 0, 50, 3, 1700000000),
            )
            conn.execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, pickaxe_tier, last_dig_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (2, 999, 100, 5, 1700001000),
            )
            # No last_dig_at — must fall back to "now"
            conn.execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, pickaxe_tier) "
                "VALUES (?, ?, ?, ?)",
                (3, 0, 25, 0),
            )
            conn.commit()
        finally:
            conn.close()

        self._rerun_gear_migration(repo_db_path)

        conn = sqlite3.connect(repo_db_path)
        try:
            rows = conn.execute(
                "SELECT discord_id, guild_id, slot, tier, durability, equipped, source "
                "FROM dig_gear ORDER BY discord_id"
            ).fetchall()
        finally:
            conn.close()

        assert len(rows) == 3
        for row in rows:
            assert row[2] == "weapon"      # slot
            assert row[4] == 20            # durability
            assert row[5] == 1             # equipped
            assert row[6] == "migration"   # source
        # Tier matches the seeded pickaxe_tier
        assert rows[0][3] == 3
        assert rows[1][3] == 5
        assert rows[2][3] == 0

    def test_initialize_is_idempotent(self, repo_db_path):
        """Re-running initialize() must not re-execute the gear backfill.

        ``schema_migrations`` is the idempotence guard — once a migration's
        name is in there, the body is skipped on subsequent runs. Without
        this guarantee a second initialize() would try to re-INSERT and
        violate the partial-unique-index.
        """
        # Already initialized via the repo_db_path fixture — adding a
        # tunnel here doesn't trigger backfill since the migration has
        # already run once. Calling initialize() a second time must be a
        # no-op for the gear migration.
        conn = sqlite3.connect(repo_db_path)
        try:
            conn.execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, pickaxe_tier) "
                "VALUES (?, ?, ?, ?)",
                (42, 0, 30, 2),
            )
            conn.commit()
        finally:
            conn.close()
        # Second initialize: migration is already recorded, so the body
        # never runs. The new tunnel does NOT get backfilled (that's the
        # cost of running the migration only once at upgrade time).
        SchemaManager(repo_db_path).initialize()

        conn = sqlite3.connect(repo_db_path)
        try:
            n = conn.execute(
                "SELECT COUNT(*) FROM dig_gear WHERE discord_id = 42"
            ).fetchone()[0]
        finally:
            conn.close()
        assert n == 0  # not backfilled because migration ran before tunnel insert


class TestMissingDigWeaponBackfillMigration:
    """Covers _migration_backfill_missing_dig_weapon_gear."""

    def test_backfills_only_tunnels_without_any_weapon_row(self, repo_db_path):
        conn = sqlite3.connect(repo_db_path)
        try:
            conn.execute("DELETE FROM tunnels")
            conn.execute("DELETE FROM dig_gear")
            conn.execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, pickaxe_tier) "
                "VALUES (?, ?, ?, ?)",
                (1, 0, 80, 3),
            )
            conn.execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, pickaxe_tier) "
                "VALUES (?, ?, ?, ?)",
                (2, 0, 50, 2),
            )
            conn.execute(
                """
                INSERT INTO dig_gear
                    (discord_id, guild_id, slot, tier, durability, equipped,
                     acquired_at, source)
                VALUES (?, ?, 'weapon', ?, 20, 0, 0, 'shop')
                """,
                (2, 0, 2),
            )
            conn.commit()
            cursor = conn.cursor()
            SchemaManager(repo_db_path)._migration_backfill_missing_dig_weapon_gear(cursor)
            conn.commit()
            rows = conn.execute(
                "SELECT discord_id, slot, tier, equipped, source "
                "FROM dig_gear ORDER BY discord_id, source"
            ).fetchall()
        finally:
            conn.close()

        assert rows == [
            (1, "weapon", 3, 1, "missing_weapon_backfill"),
            (2, "weapon", 2, 0, "shop"),
        ]
