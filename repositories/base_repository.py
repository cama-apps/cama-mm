"""
Base repository with common database operations.
"""

import sqlite3
import logging
from abc import ABC
from contextlib import contextmanager
from typing import Optional

from database import Database

logger = logging.getLogger('cama_bot.repositories')


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
    
    def get_connection(self) -> sqlite3.Connection:
        """Get database connection with row factory enabled."""
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row
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
    def cursor(self):
        """
        Context manager that yields a cursor with automatic connection management.
        """
        with self.connection() as conn:
            yield conn.cursor()

