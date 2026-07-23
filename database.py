"""SQLite runtime lifecycle and schema initialization."""

import logging
import os
import sqlite3
import uuid
from contextlib import contextmanager

from infrastructure.schema_manager import SchemaManager

logger = logging.getLogger("cama_bot.database")

DEFAULT_DB_PATH = "cama_shuffle.db"


class Database:
    """Own the database lifecycle; repositories own data access."""

    def __init__(self, db_path: str | None = None):
        raw_path = db_path or os.getenv("DB_PATH", DEFAULT_DB_PATH)
        self._is_memory = raw_path == ":memory:"
        self._memory_connection: sqlite3.Connection | None = None
        self._anchor_connection: sqlite3.Connection | None = None
        self._use_uri = False

        if self._is_memory:
            unique_name = uuid.uuid4().hex
            self.db_path = f"file:memdb_{unique_name}?mode=memory&cache=shared"
            self._use_uri = True
            self._memory_connection = self._open_connection()
        else:
            self.db_path = raw_path

        logger.info("Using database path: %s", self.db_path)
        self.schema_manager = SchemaManager(self.db_path, use_uri=self._use_uri)
        self.init_database()

        if not self._is_memory:
            self._anchor_connection = self._open_connection()

    def _open_connection(self) -> sqlite3.Connection:
        conn = sqlite3.connect(
            self.db_path,
            uri=self._use_uri,
            check_same_thread=not self._is_memory,
            timeout=5.0,
        )
        conn.row_factory = sqlite3.Row
        # SchemaManager establishes persistent WAL mode before file-backed
        # runtime connections are opened. ``timeout`` installs the equivalent
        # five-second busy handler without issuing setup SQL per connection.
        return conn

    def get_connection(self) -> sqlite3.Connection:
        """Return the shared memory connection or a new file connection."""
        if self._is_memory:
            if self._memory_connection is None:
                self._memory_connection = self._open_connection()
            return self._memory_connection
        return self._open_connection()

    @contextmanager
    def connection(self):
        """Commit on success, roll back on failure, and close file connections."""
        conn = self.get_connection()
        should_close = not self._is_memory
        try:
            yield conn
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        finally:
            if should_close:
                conn.close()

    @contextmanager
    def atomic_transaction(self):
        """Open a transaction while holding SQLite's immediate write lock."""
        conn = self.get_connection()
        should_close = not self._is_memory
        try:
            conn.execute("BEGIN IMMEDIATE")
            yield conn
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        finally:
            if should_close:
                conn.close()

    def init_database(self) -> None:
        """Initialize the schema idempotently."""
        self.schema_manager.initialize()

    def close(self) -> None:
        """Close connections kept alive for memory persistence and WAL lifetime."""
        for attribute in ("_memory_connection", "_anchor_connection"):
            connection = getattr(self, attribute)
            if connection is not None:
                connection.close()
                setattr(self, attribute, None)
