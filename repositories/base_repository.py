"""
Base repository with common database operations.
"""

import logging
import sqlite3
from abc import ABC
from contextlib import contextmanager

from database import Database

logger = logging.getLogger("cama_bot.repositories")


class BaseRepository(ABC):
    """
    Base class for all repositories.

    Provides common database connection management and utilities.
    """

    # Track DB paths that have already had schema initialization performed
    _schema_initialized_paths = set()

    def __init__(self, db_path: str):
        """
        Initialize repository with database path.

        Args:
            db_path: Path to SQLite database file
        """
        self.db_path = db_path
        # Ensure schema is initialized for this database path (idempotent)
        if db_path not in type(self)._schema_initialized_paths:
            Database(db_path)
            type(self)._schema_initialized_paths.add(db_path)

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

    def get_connection(self) -> sqlite3.Connection:
        """Get database connection with row factory enabled."""
        conn = sqlite3.connect(self.db_path)
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
