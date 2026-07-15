"""
Base repository with common database operations.
"""

import json
import logging
import sqlite3
import threading
from abc import ABC
from contextlib import contextmanager
from typing import Any

from infrastructure.schema_manager import SchemaManager

logger = logging.getLogger("cama_bot.repositories")


def safe_json_loads(raw: Any, default: Any, *, context: str = "") -> Any:
    """Parse a JSON column value, falling back to ``default`` on corruption.

    Logs a warning with ``context`` when the raw value is missing or malformed
    so that schema/data issues are visible without crashing the read path.
    """
    if raw is None or raw == "":
        return default
    try:
        return json.loads(raw)
    except (json.JSONDecodeError, TypeError) as exc:
        logger.warning(
            "Corrupt JSON column (%s): %r — falling back to default. Error: %s",
            context or "unknown",
            raw,
            exc,
        )
        return default


class BaseRepository(ABC):
    """
    Base class for all repositories.

    Provides common database connection management and utilities.
    """

    # Track DB paths that have already had schema initialization performed.
    # Guarded by _schema_init_lock so two threads can't race on the same fresh
    # path and each kick off a concurrent SchemaManager.initialize() run.
    _schema_initialized_paths = set()
    _schema_init_lock = threading.Lock()

    def __init__(self, db_path: str):
        """
        Initialize repository with database path.

        Args:
            db_path: Path to SQLite database file
        """
        self.db_path = db_path
        # _schema_initialized_paths is shared across ALL subclasses via the class variable
        # on BaseRepository — not per-subclass. Use BaseRepository explicitly to make that
        # sharing visible; using type(self) would imply per-subclass tracking, which is wrong.
        if db_path not in BaseRepository._schema_initialized_paths:
            with BaseRepository._schema_init_lock:
                if db_path not in BaseRepository._schema_initialized_paths:
                    SchemaManager(
                        db_path,
                        use_uri=db_path.startswith("file:"),
                    ).initialize()
                    BaseRepository._schema_initialized_paths.add(db_path)

    @staticmethod
    def normalize_guild_id(guild_id: int | None) -> int:
        """
        Normalize guild_id for database storage.

        Converts None to 0 for consistent storage. This allows using
        guild_id=None for DMs or tests while maintaining proper indexing.

        Args:
            guild_id: Discord guild ID or None

        Returns:
            The guild_id if not None, otherwise 0
        """
        return guild_id if guild_id is not None else 0

    def _set_economy_ledger_context(
        self,
        cursor: sqlite3.Cursor,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: Any | None = None,
    ) -> None:
        """Set trigger context for subsequent balance ledger entries."""
        metadata_json = None
        if metadata is not None:
            metadata_json = metadata if isinstance(metadata, str) else json.dumps(metadata)
        cursor.execute("DELETE FROM economy_ledger_context")
        cursor.execute(
            """
            INSERT INTO economy_ledger_context (
                id, source, actor_id, related_type, related_id, reason, metadata
            )
            VALUES (1, ?, ?, ?, ?, ?, ?)
            """,
            (
                source,
                actor_id,
                related_type,
                str(related_id) if related_id is not None else None,
                reason,
                metadata_json,
            ),
        )

    def _clear_economy_ledger_context(self, cursor: sqlite3.Cursor) -> None:
        """Clear trigger context after contextual ledger writes."""
        cursor.execute("DELETE FROM economy_ledger_context")

    def get_connection(self) -> sqlite3.Connection:
        """Get database connection with row factory enabled."""
        conn = sqlite3.connect(
            self.db_path,
            uri=self.db_path.startswith("file:"),
            check_same_thread=not self.db_path.startswith("file:"),
        )
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA busy_timeout=5000")
        return conn

    @contextmanager
    def connection(self):
        """
        Context manager for database connections.

        Automatically commits on success, rolls back on exception,
        and always closes the connection.
        """
        conn = self.get_connection()
        try:
            yield conn
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        finally:
            conn.close()

    @contextmanager
    def atomic_transaction(self):
        """
        Context manager for atomic transactions with immediate write lock.

        Uses BEGIN IMMEDIATE to acquire a write lock immediately, preventing
        concurrent writes from interleaving. This is essential for operations
        like betting where race conditions could cause double-spending.

        Usage:
            with self.atomic_transaction() as conn:
                cursor = conn.cursor()
                # Perform atomic operations
                cursor.execute(...)

        The transaction commits on success and rolls back on exception.
        """
        conn = self.get_connection()
        try:
            cursor = conn.cursor()
            cursor.execute("BEGIN IMMEDIATE")
            yield conn
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        finally:
            conn.close()

    @contextmanager
    def cursor(self):
        """
        Context manager that yields a cursor with automatic connection management.
        """
        with self.connection() as conn:
            yield conn.cursor()
