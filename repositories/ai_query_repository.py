"""
AI Query Repository for safe read-only SQL execution.

This repository enforces read-only access at the database connection level
for AI-generated queries.
"""

from __future__ import annotations

import logging
import re
import sqlite3
from contextlib import contextmanager

from repositories.base_repository import BaseRepository
from repositories.interfaces import IAIQueryRepository

logger = logging.getLogger("cama_bot.repositories.ai_query")


class AIQueryRepository(BaseRepository, IAIQueryRepository):
    """
    Repository for executing AI-generated SQL queries safely.

    Enforces read-only access via PRAGMA query_only and limits result sets.
    """

    def __init__(self, db_path: str):
        super().__init__(db_path)
        self._guild_scoped_tables: set[str] | None = None

    @contextmanager
    def readonly_connection(self):
        """
        Context manager for read-only database connections.

        Sets PRAGMA query_only = ON to prevent any write operations.
        """
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row
        try:
            # Enable read-only mode at the connection level
            conn.execute("PRAGMA query_only = ON")
            yield conn
        finally:
            conn.close()

    def get_guild_scoped_tables(self) -> set[str]:
        """
        Return the set of base tables that carry a guild_id column.

        These are the tables AI queries must be restricted to the asking guild.
        Tables without a guild_id (intentionally global, e.g. player_steam_ids)
        are excluded. Result is cached — the schema is static at runtime.
        """
        if self._guild_scoped_tables is not None:
            return self._guild_scoped_tables

        scoped: set[str] = set()
        with self.readonly_connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
            tables = [row["name"] for row in cursor.fetchall()]
            for table in tables:
                cursor.execute(f"PRAGMA table_info({table})")
                if any(col["name"] == "guild_id" for col in cursor.fetchall()):
                    scoped.add(table)

        self._guild_scoped_tables = scoped
        return scoped

    @contextmanager
    def _guild_scoped_connection(self, guild_id: int):
        """
        Read-only connection where every guild-scoped base table is shadowed by a
        TEMP VIEW filtered to ``guild_id``.

        Unqualified table references (the only kind the validator permits) resolve
        to the views, so the query sees only the asking guild's rows. Views are
        created BEFORE ``PRAGMA query_only = ON`` because query_only forbids
        creating them.
        """
        gid = int(guild_id)
        scoped_tables = self.get_guild_scoped_tables()
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row
        try:
            for table in scoped_tables:
                # Table names come from sqlite_master introspection (trusted, not
                # user input) and gid is coerced to int, so both are safe to
                # interpolate into the view definition (which cannot be parameterized).
                conn.execute(
                    f'CREATE TEMP VIEW "{table}" AS '
                    f'SELECT * FROM main."{table}" WHERE guild_id = {gid}'
                )
            conn.execute("PRAGMA query_only = ON")
            yield conn
        finally:
            conn.close()

    def execute_readonly(
        self,
        sql: str,
        params: tuple = (),
        max_rows: int = 25,
    ) -> list[dict]:
        """
        Execute a validated SQL query in read-only mode.

        Args:
            sql: The SQL query to execute (must be pre-validated)
            params: Query parameters for parameterized queries
            max_rows: Maximum number of rows to return

        Returns:
            List of dicts with column names as keys

        Raises:
            sqlite3.Error: If the query fails or attempts to write
        """
        with self.readonly_connection() as conn:
            cursor = conn.cursor()
            try:
                cursor.execute(sql, params)
                rows = cursor.fetchmany(max_rows)
                # Convert Row objects to dicts for JSON serialization
                return [dict(row) for row in rows]
            except sqlite3.Error as e:
                logger.error(f"Read-only query failed: {e}\nSQL: {sql}")
                raise

    def execute_readonly_guild_scoped(
        self,
        sql: str,
        guild_id: int | None,
        params: tuple = (),
        max_rows: int = 25,
    ) -> list[dict]:
        """
        Execute a validated SELECT with rows restricted to ``guild_id``.

        Guild isolation is enforced at the data layer (per-guild temp views), not
        by trusting the generated SQL — the model never sees the guild_id column.
        Tables without a guild_id column (intentionally global) are read unscoped.

        Args:
            sql: The SQL query to execute (must be pre-validated)
            guild_id: Asking guild; None is normalized to 0
            params: Query parameters for parameterized queries
            max_rows: Maximum number of rows to return

        Returns:
            List of dicts with column names as keys

        Raises:
            sqlite3.Error: If the query fails or attempts to write
        """
        normalized_guild = guild_id if guild_id is not None else 0
        with self._guild_scoped_connection(normalized_guild) as conn:
            cursor = conn.cursor()
            try:
                cursor.execute(sql, params)
                rows = cursor.fetchmany(max_rows)
                return [dict(row) for row in rows]
            except sqlite3.Error as e:
                logger.error(f"Guild-scoped read-only query failed: {e}\nSQL: {sql}")
                raise

    def get_table_schema(self, table_name: str) -> list[dict]:
        """
        Get schema information for a table.

        Args:
            table_name: Name of the table

        Returns:
            List of column info dicts with cid, name, type, notnull, dflt_value, pk
        """
        if not re.match(r'^[a-zA-Z_][a-zA-Z0-9_]*$', table_name):
            raise ValueError(f"Invalid table name: {table_name}")
        with self.readonly_connection() as conn:
            cursor = conn.cursor()
            cursor.execute(f"PRAGMA table_info({table_name})")
            return [dict(row) for row in cursor.fetchall()]

    def get_all_tables(self) -> list[str]:
        """
        Get list of all tables in the database.

        Returns:
            List of table names
        """
        with self.readonly_connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
            return [row["name"] for row in cursor.fetchall()]

    def get_schema_metadata(self) -> dict[str, dict[str, list[dict]]]:
        """Load table schemas and foreign keys through one read-only connection.

        Table insertion order matches :meth:`get_all_tables`; each column and
        foreign-key list retains the order returned by its SQLite PRAGMA. The
        snapshot includes every non-internal base table so callers can apply
        their own allow/block policy without opening additional connections.
        """
        with self.readonly_connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
            )
            table_names = [row["name"] for row in cursor.fetchall()]

            metadata: dict[str, dict[str, list[dict]]] = {}
            for table_name in table_names:
                # Names originate from sqlite_master rather than user input.
                # Quoting still handles any unusual but valid SQLite identifier.
                quoted_name = table_name.replace('"', '""')
                cursor.execute(f'PRAGMA table_info("{quoted_name}")')
                columns = [dict(row) for row in cursor.fetchall()]
                cursor.execute(f'PRAGMA foreign_key_list("{quoted_name}")')
                foreign_keys = [dict(row) for row in cursor.fetchall()]
                metadata[table_name] = {
                    "columns": columns,
                    "foreign_keys": foreign_keys,
                }

            return metadata

    def get_foreign_keys(self, table_name: str) -> list[dict]:
        """
        Get foreign key relationships for a table.

        Args:
            table_name: Name of the table

        Returns:
            List of FK info dicts with id, seq, table, from, to, on_update, on_delete, match
        """
        if not re.match(r'^[a-zA-Z_][a-zA-Z0-9_]*$', table_name):
            raise ValueError(f"Invalid table name: {table_name}")
        with self.readonly_connection() as conn:
            cursor = conn.cursor()
            cursor.execute(f"PRAGMA foreign_key_list({table_name})")
            return [dict(row) for row in cursor.fetchall()]
