"""
Repository for dig minigame data access.
"""

import json
import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import IDigRepository


class DigRepository(BaseRepository, IDigRepository):
    """Data access for dig tunnels, actions, inventory, artifacts, and achievements."""

    # Integer columns in the tunnels table — ensure these are always int
    # even if SQLite returns them as strings (e.g., after ALTER TABLE migrations).
    _TUNNEL_INT_COLS = frozenset({
        "discord_id", "guild_id", "depth", "max_depth", "total_digs",
        "total_jc_earned", "last_dig_at", "streak_days", "pickaxe_tier",
        "prestige_level", "trap_active", "trap_free_today", "insured_until",
        "reinforced_until", "paid_digs_today", "revenge_target", "revenge_until",
        "hard_hat_charges", "void_bait_digs", "luminosity", "boss_attempts",
        "best_run_score", "current_run_jc", "current_run_artifacts",
        "current_run_events", "total_prestige_score",
    })

    @staticmethod
    def _normalize_tunnel(row: dict) -> dict:
        """Cast integer columns to int to guard against SQLite type drift."""
        for col in DigRepository._TUNNEL_INT_COLS:
            val = row.get(col)
            if val is not None and not isinstance(val, int):
                row[col] = int(val)
        return row

    # ── Tunnel CRUD ──────────────────────────────────────────────────────

    def get_tunnel(self, discord_id: int, guild_id: int) -> dict | None:
        """Get tunnel data for a player."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM tunnels WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            row = cursor.fetchone()
            if row is None:
                return None
            return self._normalize_tunnel(dict(row))

    def create_tunnel(self, discord_id: int, guild_id: int, tunnel_name: str = None, *, name: str = None) -> dict:
        """Create a new tunnel and return it."""
        tunnel_name = tunnel_name or name or "Unnamed Tunnel"
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO tunnels (discord_id, guild_id, depth, max_depth, total_digs,
                    total_jc_earned, last_dig_at, streak_days, streak_last_date,
                    pickaxe_tier, prestige_level, prestige_perks, tunnel_name,
                    boss_progress, boss_attempts, trap_active, trap_free_today,
                    trap_date, insured_until, reinforced_until, injury_state,
                    paid_digs_today, paid_dig_date, revenge_target, revenge_type,
                    revenge_until, created_at, hard_hat_charges, void_bait_digs, cheer_data)
                VALUES (?, ?, 0, 0, 0, 0, NULL, 0, NULL, 0, 0, NULL, ?, NULL, NULL, 0, 1,
                    NULL, NULL, NULL, NULL, 0, NULL, NULL, NULL, NULL, ?, 0, 0, NULL)
                """,
                (discord_id, gid, tunnel_name, now),
            )
            # Return the newly created tunnel
            cursor.execute(
                "SELECT * FROM tunnels WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            return self._normalize_tunnel(dict(cursor.fetchone()))

    #: Columns that ``update_tunnel`` is allowed to write. Keep in sync with
    #: ``infrastructure/schema_manager.py`` CREATE TABLE tunnels and all
    #: subsequent ``_add_column_if_not_exists`` migrations for this table.
    _TUNNEL_UPDATABLE_COLUMNS: frozenset[str] = frozenset({
        # Original CREATE TABLE columns (excluding PK discord_id / guild_id and created_at)
        "depth", "max_depth", "total_digs", "total_jc_earned",
        "last_dig_at", "streak_days", "streak_last_date",
        "pickaxe_tier", "prestige_level", "prestige_perks", "tunnel_name",
        "boss_progress", "boss_attempts",
        "trap_active", "trap_free_today", "trap_date",
        "insured_until", "reinforced_until", "injury_state",
        "paid_digs_today", "paid_dig_date",
        "revenge_target", "revenge_type", "revenge_until",
        "hard_hat_charges", "cheer_data",
        # dig_expansion_luminosity_and_buffs migration
        "luminosity", "temp_buffs",
        # dig_prestige_events_columns migration
        "best_run_score", "current_run_jc", "current_run_artifacts",
        "current_run_events", "total_prestige_score", "mutations",
        # dig_void_bait_column migration
        "void_bait_digs",
        # dig_thick_skin_date migration
        "thick_skin_date",
    })

    def update_tunnel(self, discord_id: int, guild_id: int, **kwargs) -> None:
        """Update a fixed whitelist of tunnel columns.

        Raises ``ValueError`` if a kwarg is not in
        :attr:`_TUNNEL_UPDATABLE_COLUMNS`. This replaces the old
        f-string-from-kwargs path so a caller typo fails loudly instead of
        silently running an UPDATE with an unknown column (SQLite error) or,
        worse, a column that happens to exist but isn't meant to be mutated.
        """
        if not kwargs:
            return
        unknown = set(kwargs) - self._TUNNEL_UPDATABLE_COLUMNS
        if unknown:
            raise ValueError(
                f"update_tunnel got unknown columns: {sorted(unknown)}. "
                f"Add them to _TUNNEL_UPDATABLE_COLUMNS (and a migration) if valid."
            )
        gid = self.normalize_guild_id(guild_id)
        set_clauses = ", ".join(f"{col} = ?" for col in kwargs)
        values = list(kwargs.values())
        values.extend([discord_id, gid])
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"UPDATE tunnels SET {set_clauses} WHERE discord_id = ? AND guild_id = ?",
                values,
            )

    def get_leaderboard(self, guild_id: int, limit: int = 10) -> list[dict]:
        """Get top tunnels by depth (descending)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM tunnels
                WHERE guild_id = ?
                ORDER BY depth DESC
                LIMIT ?
                """,
                (gid, limit),
            )
            return [self._normalize_tunnel(dict(row)) for row in cursor.fetchall()]

    def get_player_rank(self, discord_id: int, guild_id: int) -> int:
        """Get player's rank by depth (1-indexed). Returns 0 if not found."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COUNT(*) + 1 as rank
                FROM tunnels
                WHERE guild_id = ? AND depth > (
                    SELECT COALESCE(depth, 0) FROM tunnels
                    WHERE discord_id = ? AND guild_id = ?
                )
                """,
                (gid, discord_id, gid),
            )
            row = cursor.fetchone()
            # If the player doesn't exist, the subquery returns NULL and rank is meaningless
            cursor.execute(
                "SELECT 1 FROM tunnels WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            if not cursor.fetchone():
                return 0
            return row["rank"]

    def get_all_tunnels(self, guild_id: int) -> list[dict]:
        """Get all tunnels for a guild."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM tunnels WHERE guild_id = ?",
                (gid,),
            )
            return [self._normalize_tunnel(dict(row)) for row in cursor.fetchall()]

    # ── Weather ──────────────────────────────────────────────────────────

    def get_weather(self, guild_id: int, game_date: str) -> list[dict]:
        """Get all active weather entries for a guild on a given game date."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM dig_weather WHERE guild_id = ? AND game_date = ?",
                (gid, game_date),
            )
            return [dict(row) for row in cursor.fetchall()]

    def set_weather(self, guild_id: int, game_date: str, layer_name: str, weather_id: str) -> None:
        """Set weather for a layer on a given game date."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """INSERT OR REPLACE INTO dig_weather (guild_id, game_date, layer_name, weather_id)
                   VALUES (?, ?, ?, ?)""",
                (gid, game_date, layer_name, weather_id),
            )
            conn.commit()

    # ── Action Logging ───────────────────────────────────────────────────

    def log_action(
        self,
        guild_id: int = None,
        actor_id: int = None,
        target_id: int | None = None,
        action_type: str = None,
        depth_before: int = 0,
        depth_after: int = 0,
        jc_delta: int = 0,
        detail: dict | None = None,
        *,
        discord_id: int | None = None,
        details: str | None = None,
    ) -> int:
        """Log an action and return the action_id.

        Accepts both positional style (guild_id, actor_id, ...) and
        keyword style (discord_id=, details=) for flexibility.
        """
        # Support discord_id as alias for actor_id
        if actor_id is None and discord_id is not None:
            actor_id = discord_id
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        # Support details (JSON string) as alias for detail (dict)
        if detail is not None:
            detail_json = json.dumps(detail) if isinstance(detail, dict) else detail
        elif details is not None:
            detail_json = details if isinstance(details, str) else json.dumps(details)
        else:
            detail_json = None
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO dig_actions
                    (guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (gid, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail_json, now),
            )
            return cursor.lastrowid

    def get_recent_actions(
        self,
        discord_id: int,
        guild_id: int,
        limit: int = 5,
        *,
        action_type: str | None = None,
        hours: int | None = None,
    ) -> list[dict]:
        """Get recent actions for a player (as actor OR target).

        Optional filters:
        - action_type: filter to a specific action type
        - hours: only return actions within the last N hours
        """
        gid = self.normalize_guild_id(guild_id)
        clauses = ["guild_id = ?", "(actor_id = ? OR target_id = ?)"]
        params: list = [gid, discord_id, discord_id]

        if action_type is not None:
            clauses.append("action_type = ?")
            params.append(action_type)

        if hours is not None:
            clauses.append("created_at >= ?")
            params.append(int(time.time()) - hours * 3600)

        params.append(limit)
        where = " AND ".join(clauses)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT * FROM dig_actions
                WHERE {where}
                ORDER BY created_at DESC
                LIMIT ?
                """,
                params,
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_sabotage_history(
        self, actor_id: int, target_id: int, guild_id: int, since_ts: int,
    ) -> list[dict]:
        """Get sabotage actions from actor to target since timestamp."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_actions
                WHERE guild_id = ? AND actor_id = ? AND target_id = ?
                  AND action_type LIKE 'sabotage%'
                  AND created_at >= ?
                ORDER BY created_at DESC
                """,
                (gid, actor_id, target_id, since_ts),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_helper_actions(self, target_id: int, guild_id: int, since_ts: int) -> list[dict]:
        """Get help actions targeting this player since timestamp."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_actions
                WHERE guild_id = ? AND target_id = ?
                  AND action_type LIKE 'help%'
                  AND created_at >= ?
                ORDER BY created_at DESC
                """,
                (gid, target_id, since_ts),
            )
            return [dict(row) for row in cursor.fetchall()]

    # ── Inventory ────────────────────────────────────────────────────────

    def get_inventory(self, discord_id: int, guild_id: int) -> list[dict]:
        """Get all items in inventory."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_inventory
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY created_at DESC
                """,
                (discord_id, gid),
            )
            return [dict(row) for row in cursor.fetchall()]

    def add_item(self, discord_id: int, guild_id: int, item_type: str) -> int:
        """Add item to inventory, return its id."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO dig_inventory (discord_id, guild_id, item_type, queued, created_at)
                VALUES (?, ?, ?, 0, ?)
                """,
                (discord_id, gid, item_type, now),
            )
            return cursor.lastrowid

    def remove_item(self, item_id: int) -> None:
        """Remove item by id."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM dig_inventory WHERE id = ?", (item_id,))

    def get_queued_items(self, discord_id: int, guild_id: int) -> list[dict]:
        """Get items where queued=1."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_inventory
                WHERE discord_id = ? AND guild_id = ? AND queued = 1
                ORDER BY created_at DESC
                """,
                (discord_id, gid),
            )
            return [dict(row) for row in cursor.fetchall()]

    def queue_item(self, item_id: int) -> None:
        """Set queued=1 for an item."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE dig_inventory SET queued = 1 WHERE id = ?",
                (item_id,),
            )

    def unqueue_all(self, discord_id: int, guild_id: int) -> None:
        """Set all queued=0 for a player."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE dig_inventory SET queued = 0 WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )

    def count_items(self, discord_id: int, guild_id: int) -> int:
        """Total items count for a player."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COUNT(*) FROM dig_inventory WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            return cursor.fetchone()[0]

    # ── Artifacts ────────────────────────────────────────────────────────

    def add_artifact(
        self, discord_id: int, guild_id: int, artifact_id: str, is_relic: bool = False,
    ) -> int:
        """Add artifact to player, return its db id."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO dig_artifacts (discord_id, guild_id, artifact_id, found_at, is_relic, equipped)
                VALUES (?, ?, ?, ?, ?, 0)
                """,
                (discord_id, gid, artifact_id, now, int(is_relic)),
            )
            return cursor.lastrowid

    def get_artifacts(self, discord_id: int, guild_id: int) -> list[dict]:
        """Get all artifacts for a player."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_artifacts
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY found_at DESC
                """,
                (discord_id, gid),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_equipped_relics(self, discord_id: int, guild_id: int) -> list[dict]:
        """Get equipped relics only."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_artifacts
                WHERE discord_id = ? AND guild_id = ? AND is_relic = 1 AND equipped = 1
                ORDER BY found_at DESC
                """,
                (discord_id, gid),
            )
            return [dict(row) for row in cursor.fetchall()]

    def equip_relic(self, artifact_db_id: int, equipped: bool = True) -> None:
        """Set equipped flag on a relic."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE dig_artifacts SET equipped = ? WHERE id = ?",
                (int(equipped), artifact_db_id),
            )

    def unequip_relic(self, artifact_db_id: int) -> None:
        """Unequip a relic."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE dig_artifacts SET equipped = 0 WHERE id = ?",
                (artifact_db_id,),
            )

    def count_equipped_relics(self, discord_id: int, guild_id: int) -> int:
        """Count equipped relics for a player."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COUNT(*) FROM dig_artifacts
                WHERE discord_id = ? AND guild_id = ? AND is_relic = 1 AND equipped = 1
                """,
                (discord_id, gid),
            )
            return cursor.fetchone()[0]

    def remove_artifact(self, artifact_db_id: int) -> None:
        """Remove an artifact (for gifting)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM dig_artifacts WHERE id = ?", (artifact_db_id,))

    def has_artifact(self, discord_id: int, guild_id: int, artifact_id: str) -> bool:
        """Check if player has a specific artifact."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT 1 FROM dig_artifacts
                WHERE discord_id = ? AND guild_id = ? AND artifact_id = ?
                LIMIT 1
                """,
                (discord_id, gid, artifact_id),
            )
            return cursor.fetchone() is not None

    # ── Artifact Registry ────────────────────────────────────────────────

    def register_artifact_find(
        self, artifact_id: str, guild_id: int, finder_id: int, found_at: int,
    ) -> bool:
        """Register an artifact find. Returns True if this is the first finder."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM dig_artifact_registry WHERE artifact_id = ? AND guild_id = ?",
                (artifact_id, gid),
            )
            existing = cursor.fetchone()
            if existing:
                cursor.execute(
                    """
                    UPDATE dig_artifact_registry
                    SET total_found = total_found + 1
                    WHERE artifact_id = ? AND guild_id = ?
                    """,
                    (artifact_id, gid),
                )
                return False
            else:
                cursor.execute(
                    """
                    INSERT INTO dig_artifact_registry
                        (artifact_id, guild_id, first_finder_id, first_found_at, total_found)
                    VALUES (?, ?, ?, ?, 1)
                    """,
                    (artifact_id, gid, finder_id, found_at),
                )
                return True

    def get_registry(self, guild_id: int) -> list[dict]:
        """Get all registered artifacts for a guild."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_artifact_registry
                WHERE guild_id = ?
                ORDER BY first_found_at ASC
                """,
                (gid,),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_registry_entry(self, artifact_id: str, guild_id: int) -> dict | None:
        """Get a single registry entry."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM dig_artifact_registry WHERE artifact_id = ? AND guild_id = ?",
                (artifact_id, gid),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    # ── Achievements ─────────────────────────────────────────────────────

    def add_achievement(
        self, discord_id: int, guild_id: int, achievement_id: str, unlocked_at: int,
    ) -> bool:
        """Add achievement if not exists. Returns True if newly added."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT OR IGNORE INTO dig_achievements
                    (discord_id, guild_id, achievement_id, unlocked_at)
                VALUES (?, ?, ?, ?)
                """,
                (discord_id, gid, achievement_id, unlocked_at),
            )
            return cursor.rowcount > 0

    def get_achievements(self, discord_id: int, guild_id: int) -> list[dict]:
        """Get all achievements for a player."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_achievements
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY unlocked_at ASC
                """,
                (discord_id, gid),
            )
            return [dict(row) for row in cursor.fetchall()]

    def has_achievement(self, discord_id: int, guild_id: int, achievement_id: str) -> bool:
        """Check if player has a specific achievement."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT 1 FROM dig_achievements
                WHERE discord_id = ? AND guild_id = ? AND achievement_id = ?
                LIMIT 1
                """,
                (discord_id, gid, achievement_id),
            )
            return cursor.fetchone() is not None

    # ── Atomic Operations ────────────────────────────────────────────────

    def atomic_sabotage(
        self,
        actor_id: int,
        target_id: int,
        guild_id: int,
        target_depth_delta: int,
        actor_jc_cost: int,
    ) -> dict:
        """Atomically debit actor JC, reduce target depth, return updated target tunnel."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Debit actor balance
            if actor_jc_cost > 0:
                cursor.execute(
                    "UPDATE players SET jopacoin_balance = jopacoin_balance - ? WHERE discord_id = ? AND guild_id = ?",
                    (actor_jc_cost, actor_id, gid),
                )

            # Reduce target depth (clamp to 0)
            cursor.execute(
                """
                UPDATE tunnels
                SET depth = MAX(0, depth + ?)
                WHERE discord_id = ? AND guild_id = ?
                """,
                (target_depth_delta, target_id, gid),
            )

            # Return updated target tunnel
            cursor.execute(
                "SELECT * FROM tunnels WHERE discord_id = ? AND guild_id = ?",
                (target_id, gid),
            )
            return self._normalize_tunnel(dict(cursor.fetchone()))

    # ── Aliases ───────────────────────────────────────────────────────

    def add_inventory_item(self, discord_id: int, guild_id: int, item_type: str) -> int:
        """Alias for add_item."""
        return self.add_item(discord_id, guild_id, item_type)

    def remove_inventory_item(self, discord_id: int, guild_id: int, item_type: str) -> None:
        """Remove first item of given type from inventory."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                DELETE FROM dig_inventory WHERE id = (
                    SELECT id FROM dig_inventory
                    WHERE discord_id = ? AND guild_id = ? AND item_type = ?
                    LIMIT 1
                )
                """,
                (discord_id, gid, item_type),
            )

    def get_top_tunnels(self, guild_id: int, limit: int = 10) -> list[dict]:
        """Alias for get_leaderboard."""
        return self.get_leaderboard(guild_id, limit)

    def get_hall_of_fame(self, guild_id: int) -> list[dict]:
        """Get top 10 tunnels by best_run_score for the guild."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, tunnel_name, prestige_level, best_run_score
                FROM tunnels
                WHERE guild_id = ? AND best_run_score > 0
                ORDER BY best_run_score DESC
                LIMIT 10
                """,
                (gid,),
            )
            return [dict(row) for row in cursor.fetchall()]
