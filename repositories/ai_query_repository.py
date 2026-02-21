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
