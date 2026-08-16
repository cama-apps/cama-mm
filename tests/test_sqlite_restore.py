from __future__ import annotations

import fcntl
import json
import sqlite3
from pathlib import Path

import pytest

from scripts.sqlite_backup import backup
from scripts.sqlite_restore import restore, sha256


def create_database(path: Path, value: str) -> None:
    with sqlite3.connect(path) as connection:
        connection.executescript(
            "CREATE TABLE schema_migrations(name TEXT PRIMARY KEY);"
            "CREATE TABLE marker(value TEXT NOT NULL);"
        )
        connection.execute("INSERT INTO schema_migrations(name) VALUES ('base')")
        connection.execute("INSERT INTO marker(value) VALUES (?)", (value,))


def marker(path: Path) -> str:
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as connection:
        return str(connection.execute("SELECT value FROM marker").fetchone()[0])


def test_restore_republishes_verified_backup_and_removes_rust_sidecars(
    tmp_path: Path,
) -> None:
    database = tmp_path / "cama_shuffle.db"
    saved = tmp_path / "backups" / "predeploy.db"
    create_database(database, "python-pre-cutover")
    metadata = backup(database, saved)
    saved_bytes = saved.read_bytes()
    metadata_path = Path(f"{saved}.json")
    metadata_bytes = metadata_path.read_bytes()

    with sqlite3.connect(database) as connection:
        connection.execute("UPDATE marker SET value='rust-mutated'")
    Path(f"{database}-wal").write_bytes(b"stale-rust-wal")
    Path(f"{database}-shm").write_bytes(b"stale-rust-shm")
    Path(f"{database}-journal").write_bytes(b"stale-rust-journal")

    report = restore(saved, database)

    assert report == {
        "format_version": 1,
        "restored_from": str(saved.resolve()),
        "destination": str(database.resolve()),
        "size_bytes": len(saved_bytes),
        "sha256": metadata["sha256"],
        "schema_migration_count": 1,
        "quick_check": "ok",
    }
    assert database.read_bytes() == saved_bytes
    assert marker(database) == "python-pre-cutover"
    assert not Path(f"{database}-wal").exists()
    assert not Path(f"{database}-shm").exists()
    assert not Path(f"{database}-journal").exists()
    assert saved.read_bytes() == saved_bytes
    assert metadata_path.read_bytes() == metadata_bytes


def test_restore_recreates_a_database_removed_by_the_failed_candidate(tmp_path: Path) -> None:
    database = tmp_path / "cama_shuffle.db"
    saved = tmp_path / "backups" / "predeploy.db"
    create_database(database, "python-pre-cutover")
    backup(database, saved)
    database.unlink()

    restore(saved, database)

    assert marker(database) == "python-pre-cutover"
    assert database.read_bytes() == saved.read_bytes()


def test_restore_rejects_tampered_backup_without_changing_live_database(
    tmp_path: Path,
) -> None:
    database = tmp_path / "cama_shuffle.db"
    saved = tmp_path / "backups" / "predeploy.db"
    create_database(database, "live")
    backup(database, saved)
    live_bytes = database.read_bytes()
    saved.write_bytes(saved.read_bytes() + b"tampered")

    with pytest.raises(RuntimeError, match="size does not match"):
        restore(saved, database)

    assert database.read_bytes() == live_bytes
    assert marker(database) == "live"
    assert not list(tmp_path.glob(".*.restore.partial"))


def test_restore_refuses_while_runtime_lock_is_held(tmp_path: Path) -> None:
    database = tmp_path / "cama_shuffle.db"
    saved = tmp_path / "backups" / "predeploy.db"
    create_database(database, "live")
    backup(database, saved)
    live_digest = sha256(database)
    lock_path = tmp_path / ".cama-runtime.lock"

    with lock_path.open("a+b") as lock_file:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(RuntimeError, match="runtime lock is still held"):
            restore(saved, database)

    assert sha256(database) == live_digest
    assert marker(database) == "live"


@pytest.mark.parametrize("suffix", ["-wal", "-shm", "-journal"])
def test_restore_rejects_backup_in_live_sidecar_namespace(tmp_path: Path, suffix: str) -> None:
    database = tmp_path / "cama_shuffle.db"
    ordinary_backup = tmp_path / "predeploy.db"
    sidecar_backup = Path(f"{database}{suffix}")
    create_database(database, "live")
    live_digest = sha256(database)
    metadata = backup(database, ordinary_backup)
    sidecar_backup.write_bytes(ordinary_backup.read_bytes())
    Path(f"{sidecar_backup}.json").write_text(
        json.dumps({**metadata, "backup": str(sidecar_backup.resolve())}),
        encoding="utf-8",
    )

    with pytest.raises(RuntimeError, match="overlaps the live SQLite namespace"):
        restore(sidecar_backup, database)

    assert sha256(database) == live_digest
    sidecar_backup.unlink()
    assert marker(database) == "live"


def test_restore_rejects_backup_for_a_different_database(tmp_path: Path) -> None:
    expected_database = tmp_path / "expected.db"
    wrong_database = tmp_path / "wrong.db"
    saved = tmp_path / "backups" / "predeploy.db"
    create_database(expected_database, "expected-live")
    create_database(wrong_database, "wrong-source")
    backup(wrong_database, saved)
    expected_digest = sha256(expected_database)

    with pytest.raises(RuntimeError, match="source does not match"):
        restore(saved, expected_database)

    assert sha256(expected_database) == expected_digest
    assert marker(expected_database) == "expected-live"
