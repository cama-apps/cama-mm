"""Regression tests for low-overhead SQLite runtime connections."""

import sqlite3

import database as database_module
import repositories.base_repository as base_repository_module
from database import Database
from repositories.player_repository import PlayerRepository


def _capture_connection_setup(monkeypatch, sqlite_module, opener):
    """Open a traced connection and return SQL run during setup."""
    real_connect = sqlite3.connect
    statements: list[str] = []

    def traced_connect(*args, **kwargs):
        connection = real_connect(*args, **kwargs)
        connection.set_trace_callback(statements.append)
        return connection

    monkeypatch.setattr(sqlite_module, "connect", traced_connect)
    connection = opener()
    return connection, tuple(statements)


def _assert_runtime_configuration(connection, setup_statements):
    # The point of this regression test is that opening a runtime connection
    # costs NO setup round-trips: WAL is persistent on the file and the busy
    # timeout comes from the connect() argument. Assert the statement list is
    # empty rather than naming individual pragmas, so any newly introduced
    # per-connection setup SQL fails here.
    assert setup_statements == ()
    assert connection.execute("PRAGMA journal_mode").fetchone()[0] == "wal"
    assert connection.execute("PRAGMA busy_timeout").fetchone()[0] == 5000


def test_repository_connection_reuses_schema_sqlite_configuration(
    repo_db_path, monkeypatch
):
    repository = PlayerRepository(repo_db_path)
    connection, setup_statements = _capture_connection_setup(
        monkeypatch,
        base_repository_module.sqlite3,
        # The production opener, not the suite-wide synchronous=OFF wrapper
        # conftest installs — otherwise this measures the test harness.
        lambda: base_repository_module.BaseRepository.durable_get_connection(
            repository
        ),
    )
    try:
        _assert_runtime_configuration(connection, setup_statements)
    finally:
        connection.close()


def test_database_connection_reuses_schema_sqlite_configuration(
    repo_db_path, monkeypatch
):
    database = Database(repo_db_path)
    try:
        connection, setup_statements = _capture_connection_setup(
            monkeypatch,
            database_module.sqlite3,
            database.get_connection,
        )
        try:
            _assert_runtime_configuration(connection, setup_statements)
        finally:
            connection.close()
    finally:
        database.close()
