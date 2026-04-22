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
        "current_run_events", "total_prestige_score", "stat_strength",
        "stat_smarts", "stat_stamina", "stat_points",
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
        # dig_engine_mode_column migration
        "engine_mode",
        # dig_miner_profile_columns migration
        "miner_origin", "miner_about", "stat_strength", "stat_smarts",
        "stat_stamina", "stat_points", "stat_boss_awards",
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

    def get_recent_diggers(
        self,
        guild_id: int,
        *,
        days: int = 7,
        exclude_id: int | None = None,
        limit: int | None = None,
    ) -> list[int]:
        """Return ``discord_id``s that have logged a ``dig`` action recently.

        Used by dig splash events (``active_diggers`` pool) so collateral
        events only hit players who actually dig. Draws from ``dig_actions``
        rather than ``tunnels.last_dig_at`` so lookbacks are precise and
        don't count stale tunnel timestamps.
        """
        gid = self.normalize_guild_id(guild_id)
        cutoff = int(time.time()) - max(0, days) * 86400
        params: list = [gid, cutoff]
        query = (
            "SELECT DISTINCT actor_id FROM dig_actions "
            "WHERE guild_id = ? AND action_type = 'dig' AND created_at >= ?"
        )
        if exclude_id is not None:
            query += " AND actor_id != ?"
            params.append(exclude_id)
        if limit is not None and limit > 0:
            query += " LIMIT ?"
            params.append(limit)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(query, params)
            return [row["actor_id"] for row in cursor.fetchall() if row["actor_id"] is not None]

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

    def get_player_jc_events(
        self, discord_id: int, guild_id: int | None = None
    ) -> list[dict]:
        """
        Return every dig action where ``discord_id`` is actor or target, oldest first,
        for balance-history reconstruction. Each row includes ``actor_id, target_id,
        action_type, detail, created_at, jc_delta``. The caller parses the ``detail``
        JSON to derive the per-user JC delta for action types whose JC movement isn't
        captured in the ``jc_delta`` column (which is always 0 in current code).
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT actor_id, target_id, action_type, detail, created_at, jc_delta
                FROM dig_actions
                WHERE guild_id = ? AND (actor_id = ? OR target_id = ?)
                ORDER BY created_at ASC
                """,
                (gid, discord_id, discord_id),
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

    # ── Engine Mode (legacy/llm toggle) ─────────────────────────────────

    def get_engine_mode(self, discord_id: int, guild_id: int) -> str:
        """Get the dig engine mode for a player ('legacy' or 'llm')."""
        tunnel = self.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return "legacy"
        return tunnel.get("engine_mode", "legacy") or "legacy"

    def set_engine_mode(self, discord_id: int, guild_id: int, mode: str) -> None:
        """Set the dig engine mode for a player."""
        self.update_tunnel(discord_id, guild_id, engine_mode=mode)

    # ── Personality (LLM player profiling) ──────────────────────────────

    def get_personality(self, discord_id: int, guild_id: int) -> dict | None:
        """Get player personality data for LLM context."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM dig_personality WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            row = cursor.fetchone()
            if row is None:
                return None
            result = dict(row)
            # Deserialize JSON fields
            for field in ("choice_histogram", "notable_moments"):
                raw = result.get(field)
                if raw and isinstance(raw, str):
                    try:
                        result[field] = json.loads(raw)
                    except (json.JSONDecodeError, TypeError):
                        pass
            return result

    def upsert_personality(self, discord_id: int, guild_id: int, data: dict) -> None:
        """Insert or update player personality data."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())

        histogram = data.get("choice_histogram", {})
        notable = data.get("notable_moments", [])
        histogram_json = json.dumps(histogram) if isinstance(histogram, dict) else str(histogram)
        notable_json = json.dumps(notable) if isinstance(notable, list) else str(notable)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO dig_personality
                    (discord_id, guild_id, play_style, choice_histogram,
                     notable_moments, summary, social_summary, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    play_style = excluded.play_style,
                    choice_histogram = excluded.choice_histogram,
                    notable_moments = excluded.notable_moments,
                    summary = excluded.summary,
                    social_summary = excluded.social_summary,
                    updated_at = excluded.updated_at
                """,
                (
                    discord_id, gid,
                    data.get("play_style", "unknown"),
                    histogram_json, notable_json,
                    data.get("summary", ""),
                    data.get("social_summary", ""),
                    now,
                ),
            )
            conn.commit()

    # ── Social Action Queries ───────────────────────────────────────────

    def get_recent_social_actions(
        self, discord_id: int, guild_id: int, hours: int = 48,
    ) -> list[dict]:
        """Get recent social actions involving this player (help, sabotage, cheer)."""
        gid = self.normalize_guild_id(guild_id)
        cutoff = int(time.time()) - (hours * 3600)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM dig_actions
                WHERE guild_id = ?
                  AND (actor_id = ? OR target_id = ?)
                  AND action_type IN ('sabotage', 'help', 'cheer')
                  AND created_at >= ?
                ORDER BY created_at DESC
                LIMIT 20
                """,
                (gid, discord_id, discord_id, cutoff),
            )
            return [dict(row) for row in cursor.fetchall()]

    # ── Boss echoes (post-kill weakening window) ────────────────────────

    def record_boss_echo(
        self,
        guild_id: int | None,
        boss_id: str,
        depth: int,
        killer_discord_id: int,
        window_seconds: int,
    ) -> None:
        """Upsert the echo row for (guild, boss_id) with a fresh window.

        With multiple bosses per tier, the echo is keyed by boss_id so killing
        one boss at a depth only weakens that specific boss for guildmates.
        ``depth`` is persisted alongside purely for reporting / queries.

        A BEGIN IMMEDIATE write lock is used so simultaneous kills of the
        same boss don't race; last writer wins.
        """
        gid = self.normalize_guild_id(guild_id)
        weakened_until = int(time.time()) + int(window_seconds)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO dig_boss_echoes
                    (guild_id, boss_id, depth, killer_discord_id, weakened_until)
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(guild_id, boss_id) DO UPDATE SET
                    depth             = excluded.depth,
                    killer_discord_id = excluded.killer_discord_id,
                    weakened_until    = excluded.weakened_until
                """,
                (gid, boss_id, int(depth), int(killer_discord_id), weakened_until),
            )

    def get_active_boss_echo(
        self, guild_id: int | None, boss_id: str,
    ) -> dict | None:
        """Return the active echo row for (guild, boss_id) or None if expired."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT boss_id, depth, killer_discord_id, weakened_until
                FROM dig_boss_echoes
                WHERE guild_id = ? AND boss_id = ? AND weakened_until > ?
                """,
                (gid, boss_id, now),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    # ── Active boss-duel state (mid-fight prompt persistence) ────────────

    _DUEL_COLUMNS = (
        "boss_id", "tier", "mechanic_id", "risk_tier", "wager",
        "player_hp", "boss_hp", "round_num", "round_log", "pending_prompt",
        "rng_state", "status_effects", "echo_applied", "echo_killer_id",
        "player_hit", "player_dmg", "boss_hit", "boss_dmg",
        "created_at", "last_interaction_at",
    )

    def get_active_duel(
        self, discord_id: int, guild_id: int | None,
    ) -> dict | None:
        """Return the paused mid-duel state row for a player, or None."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM dig_active_duels WHERE discord_id = ? AND guild_id = ?",
                (int(discord_id), gid),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def save_active_duel(
        self, discord_id: int, guild_id: int | None, state: dict,
    ) -> None:
        """Upsert the duel state row (one per player per guild).

        State dict keys should align with ``_DUEL_COLUMNS``. Missing keys fall
        back to current-row values (for partial updates); unknown keys are
        ignored. Wrapped in BEGIN IMMEDIATE so prompt-click races can't
        corrupt the row.
        """
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        values: dict = {}
        for col in self._DUEL_COLUMNS:
            if col in state:
                values[col] = state[col]
        values.setdefault("created_at", now)
        values["last_interaction_at"] = now

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            # Check existence for upsert semantics
            cursor.execute(
                "SELECT 1 FROM dig_active_duels WHERE discord_id = ? AND guild_id = ?",
                (int(discord_id), gid),
            )
            exists = cursor.fetchone() is not None

            if not exists:
                # Full insert — require core fields
                required = (
                    "boss_id", "tier", "mechanic_id", "risk_tier", "wager",
                    "player_hp", "boss_hp", "round_num", "rng_state",
                    "player_hit", "player_dmg", "boss_hit", "boss_dmg",
                )
                missing = [k for k in required if k not in values]
                if missing:
                    raise ValueError(f"save_active_duel insert missing: {missing}")
                values.setdefault("round_log", "[]")
                values.setdefault("status_effects", "{}")
                values.setdefault("echo_applied", 0)
                values.setdefault("echo_killer_id", None)
                cols = ["discord_id", "guild_id", *self._DUEL_COLUMNS]
                placeholders = ",".join("?" for _ in cols)
                cursor.execute(
                    f"INSERT INTO dig_active_duels ({','.join(cols)}) VALUES ({placeholders})",
                    (int(discord_id), gid, *(values.get(c) for c in self._DUEL_COLUMNS)),
                )
            else:
                # Update only provided columns (excluding created_at)
                updatable = [c for c in values if c != "created_at"]
                if not updatable:
                    return
                set_clause = ",".join(f"{c} = ?" for c in updatable)
                cursor.execute(
                    f"UPDATE dig_active_duels SET {set_clause} "
                    "WHERE discord_id = ? AND guild_id = ?",
                    (*(values[c] for c in updatable), int(discord_id), gid),
                )

    def clear_active_duel(
        self, discord_id: int, guild_id: int | None,
    ) -> None:
        """Delete the paused duel row after final resolution."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "DELETE FROM dig_active_duels WHERE discord_id = ? AND guild_id = ?",
                (int(discord_id), gid),
            )

    # ── Great Lantern ownership check ───────────────────────────────────

    def has_great_lantern(
        self, discord_id: int, guild_id: int | None,
    ) -> bool:
        """True if the player owns a 'great_lantern' item (persistent gear)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT 1 FROM dig_inventory
                WHERE discord_id = ? AND guild_id = ? AND item_type = 'great_lantern'
                LIMIT 1
                """,
                (int(discord_id), gid),
            )
            return cursor.fetchone() is not None
